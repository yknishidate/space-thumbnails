// C bridge over the MaterialX GLSL render pipeline: loads a .mtlx material,
// applies it to the MaterialX shader ball, renders offscreen (hidden window +
// WGL context) with an HDR environment light rig, and returns RGBA8 pixels.
//
// The setup mirrors MaterialXView / the MaterialXTest render harness; see
// those for reference when upgrading MaterialX.

#include <MaterialXCore/Document.h>
#include <MaterialXCore/Unit.h>
#include <MaterialXFormat/XmlIo.h>
#include <MaterialXFormat/Util.h>
#include <MaterialXGenShader/GenContext.h>
#include <MaterialXGenShader/HwShaderGenerator.h>
#include <MaterialXGenShader/Shader.h>
#include <MaterialXGenShader/Util.h>
#include <MaterialXGenShader/DefaultColorManagementSystem.h>
#include <MaterialXGenShader/UnitSystem.h>
#include <MaterialXGenGlsl/GlslShaderGenerator.h>
#include <MaterialXRender/CgltfLoader.h>
#include <MaterialXRender/GeometryHandler.h>
#include <MaterialXRender/LightHandler.h>
#include <MaterialXRender/StbImageLoader.h>
#include <MaterialXRender/TinyObjLoader.h>
#include <MaterialXRender/Util.h>
#include <MaterialXRenderGlsl/GlslMaterial.h>
#include <MaterialXRenderGlsl/GlslRenderer.h>
#include <MaterialXRenderGlsl/GLTextureHandler.h>
#include <MaterialXRenderGlsl/External/Glad/glad.h>

#include <cstdint>
#include <cstring>
#include <string>

namespace mx = MaterialX;

namespace
{

mx::DocumentPtr load_standard_library(const mx::FileSearchPath& searchPath,
                                      const std::string& dataRoot)
{
    // The server processes requests serially.  Standard definitions are
    // immutable after loading, so retain their parsed document across jobs.
    static std::string cachedDataRoot;
    static mx::DocumentPtr cachedLibrary;
    if (!cachedLibrary || cachedDataRoot != dataRoot)
    {
        cachedLibrary = mx::createDocument();
        mx::loadLibraries({ "libraries" }, searchPath, cachedLibrary);
        cachedDataRoot = dataRoot;
    }
    return cachedLibrary;
}

bool canonicalize_simple_gltf_pbr(const mx::DocumentPtr& doc)
{
    std::vector<mx::NodePtr> shaders = doc->getNodes("gltf_pbr");
    std::vector<mx::NodePtr> materials = doc->getNodes("surfacematerial");
    if (shaders.size() != 1 || materials.size() != 1)
    {
        return false;
    }
    mx::NodePtr shader = shaders[0];
    for (mx::InputPtr input : shader->getInputs())
    {
        if (input->getConnectedNode() || !input->getNodeName().empty() ||
            !input->getOutputString().empty())
        {
            return false;
        }
    }

    mx::NodePtr material = materials[0];
    mx::InputPtr surfaceInput = material->getInput("surfaceshader");
    if (!surfaceInput)
    {
        return false;
    }
    shader->setName("SR_thumbnail");
    material->setName("Material_thumbnail");
    surfaceInput->setConnectedNode(shader);
    return true;
}

void remove_public_uniform_initializers(const mx::ShaderPtr& shader)
{
    std::string source = shader->getSourceCode(mx::Stage::PIXEL);
    const size_t blockStart = source.find("// Uniform block: PublicUniforms");
    const size_t blockEnd = source.find("\nin VertexData", blockStart);
    if (blockStart == std::string::npos || blockEnd == std::string::npos)
    {
        return;
    }
    size_t lineStart = source.find('\n', blockStart) + 1;
    while (lineStart < blockEnd)
    {
        size_t lineEnd = source.find('\n', lineStart);
        size_t equals = source.find(" = ", lineStart);
        size_t semicolon = source.find(';', lineStart);
        if (equals < lineEnd && semicolon < lineEnd)
        {
            const size_t removed = semicolon - equals;
            source.erase(equals, removed);
            lineEnd -= removed;
        }
        lineStart = lineEnd + 1;
    }
    shader->setSourceCode(source, mx::Stage::PIXEL);
}

void bind_public_uniforms(const mx::GlslProgramPtr& program)
{
    const mx::VariableBlock& uniforms =
        program->getShader()->getStage(mx::Stage::PIXEL)
            .getUniformBlock(mx::HW::PUBLIC_UNIFORMS);
    for (mx::ShaderPort* uniform : uniforms.getVariableOrder())
    {
        if (uniform->getValue() && uniform->getType() != mx::Type::FILENAME &&
            uniform->getType() != mx::Type::STRING)
        {
            program->bindUniform(uniform->getVariable(), uniform->getValue(), false);
        }
    }
}

void set_error(char* err_buf, uint32_t err_buf_len, const std::string& message)
{
    if (err_buf && err_buf_len > 0)
    {
        size_t count = message.size() < err_buf_len - 1 ? message.size() : err_buf_len - 1;
        std::memcpy(err_buf, message.c_str(), count);
        err_buf[count] = '\0';
    }
}

bool generate_environment_shader(
    const mx::GlslMaterialPtr& material,
    mx::GenContext& context,
    const mx::FilePath& documentPath,
    const mx::DocumentPtr& stdLib,
    const mx::FilePath& imagePath)
{
    // Start with MaterialXView's standard environment graph, but invert the
    // view direction's Y component before its lat-long projection.  This
    // corrects the Stb/OpenGL vertical convention on the GPU without copying
    // and flipping the multi-megabyte HDR image on every request.
    mx::DocumentPtr doc = mx::createDocument();
    doc->setDataLibrary(stdLib);
    mx::readFromXmlFile(doc, documentPath);
    mx::NodeGraphPtr graph = doc->getNodeGraph("envMap");
    if (!graph)
    {
        return false;
    }
    mx::NodePtr viewDirection = graph->getNode("viewDir");
    mx::NodePtr image = graph->getNode("envImage");
    mx::OutputPtr output = graph->getOutput("out");
    if (!viewDirection || !image || !output)
    {
        return false;
    }

    mx::NodePtr flip = graph->addNode("multiply", "flipViewDirection", "vector3");
    flip->setConnectedNode("in1", viewDirection);
    flip->setInputValue("in2", mx::Vector3(1.0f, -1.0f, 1.0f));
    image->setConnectedNode("viewdir", flip);
    image->setInputValue("file", imagePath.asString(), mx::FILENAME_TYPE_STRING);

    material->setDocument(doc);
    material->setElement(output);
    return material->generateShader(context);
}

// MaterialXView's OpenGL environment-background pass, followed by the model
// pass from GlslRenderer::render.  This is used only for transparent shaders;
// opaque thumbnails retain the simpler renderer path below.
void render_with_environment(
    const mx::GlslRendererPtr& renderer,
    const mx::GlslMaterialPtr& envMaterial,
    const mx::GeometryHandlerPtr& envGeometry,
    const mx::ImageHandlerPtr& imageHandler,
    const mx::FileSearchPath& searchPath,
    const mx::GeometryHandlerPtr& geometryHandler,
    const mx::LightHandlerPtr& lightHandler)
{
    mx::GLFramebufferPtr framebuffer = renderer->getFramebuffer();
    framebuffer->bind();

    glClearColor(0.0f, 0.0f, 0.0f, 1.0f);
    glEnable(GL_DEPTH_TEST);
    glEnable(GL_FRAMEBUFFER_SRGB);
    glDepthFunc(GL_LESS);
    glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);

    // Draw the HDRI as an environment sphere, exactly as MaterialXView does.
    mx::CameraPtr envCamera = mx::Camera::create();
    // MaterialXView uses 300 units with its longer far plane.  The thumbnail
    // renderer's default far plane is 100, so keep the sphere comfortably
    // inside it while remaining effectively infinite relative to the ball.
    envCamera->setWorldMatrix(mx::Matrix44::createScale(mx::Vector3(50.0f)));
    envCamera->setViewMatrix(renderer->getCamera()->getViewMatrix());
    envCamera->setProjectionMatrix(renderer->getCamera()->getProjectionMatrix());

    const mx::MeshList& envMeshes = envGeometry->getMeshes();
    mx::MeshPartitionPtr envPart =
        !envMeshes.empty() ? envMeshes[0]->getPartition(0) : nullptr;
    if (envPart)
    {
        glDepthMask(GL_FALSE);
        envMaterial->bindShader();
        envMaterial->bindMesh(envMeshes[0]);
        envMaterial->bindViewInformation(envCamera);
        envMaterial->bindImages(imageHandler, searchPath, false);
        envMaterial->drawPartition(envPart);
        envMaterial->unbindImages(imageHandler);
        envMaterial->unbindGeometry();
        glDepthMask(GL_TRUE);
    }

    // Draw the shader ball using the same bindings and transparent two-sided
    // pass as GlslRenderer::render.
    mx::GlslProgramPtr program = renderer->getProgram();
    if (!program || !program->bind())
    {
        framebuffer->unbind();
        throw mx::ExceptionRenderError("Cannot bind material shader");
    }
    program->getUniformsList();
    program->getAttributesList();
    bind_public_uniforms(program);
    program->bindViewInformation(renderer->getCamera());
    program->bindTextures(imageHandler);
    program->bindLighting(lightHandler, imageHandler);
    program->bindTimeAndFrame();

    glEnable(GL_BLEND);
    glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
    for (mx::MeshPtr mesh : geometryHandler->getMeshes())
    {
        program->bindMesh(mesh);
        for (size_t i = 0; i < mesh->getPartitionCount(); ++i)
        {
            mx::MeshPartitionPtr part = mesh->getPartition(i);
            program->bindPartition(part);
            mx::MeshIndexBuffer& indices = part->getIndices();
            glEnable(GL_CULL_FACE);
            glCullFace(GL_FRONT);
            glDrawElements(GL_TRIANGLES, static_cast<GLsizei>(indices.size()),
                           GL_UNSIGNED_INT, nullptr);
            glCullFace(GL_BACK);
            glDisable(GL_CULL_FACE);
            glDrawElements(GL_TRIANGLES, static_cast<GLsizei>(indices.size()),
                           GL_UNSIGNED_INT, nullptr);
        }
    }
    imageHandler->unbindImages();
    program->unbind();
    glDisable(GL_BLEND);
    framebuffer->unbind();
}

} // anonymous namespace

// Renders `mtlx_path` and writes size*size*4 top-down RGBA8 bytes to out_rgba.
// data_root must contain the MaterialX "libraries/" and "resources/" trees.
// Returns 0 on success; on failure returns nonzero and fills err_buf.
extern "C" int32_t mtlx_render_thumbnail(
    const char* mtlx_path,
    const char* data_root,
    uint32_t size,
    uint8_t* out_rgba,
    char* err_buf,
    uint32_t err_buf_len)
{
    try
    {
        mx::FileSearchPath searchPath{ mx::FilePath(data_root) };

        // Standard node definition libraries.
        mx::DocumentPtr stdLib =
            load_standard_library(searchPath, std::string(data_root));

        // Material document.
        mx::DocumentPtr doc = mx::createDocument();
        mx::readFromXmlFile(doc, mtlx_path, searchPath);
        doc->setDataLibrary(stdLib);
        const bool canonicalGltfPbr = canonicalize_simple_gltf_pbr(doc);

        std::vector<mx::TypedElementPtr> elems = mx::findRenderableElements(doc);
        if (elems.empty())
        {
            set_error(err_buf, err_buf_len, "no renderable elements in document");
            return 1;
        }
        mx::TypedElementPtr elem = elems[0];

        // Shader generator with color management and units, like MaterialXView.
        mx::ShaderGeneratorPtr generator = mx::GlslShaderGenerator::create();
        mx::ColorManagementSystemPtr cms =
            mx::DefaultColorManagementSystem::create(generator->getTarget());
        cms->loadLibrary(stdLib);
        generator->setColorManagementSystem(cms);

        mx::UnitSystemPtr unitSystem = mx::UnitSystem::create(generator->getTarget());
        unitSystem->loadLibrary(stdLib);
        mx::UnitConverterRegistryPtr unitRegistry = mx::UnitConverterRegistry::create();
        if (mx::UnitTypeDefPtr distanceTypeDef = stdLib->getUnitTypeDef("distance"))
        {
            unitRegistry->addUnitConverter(distanceTypeDef,
                                           mx::LinearUnitConverter::create(distanceTypeDef));
        }
        if (mx::UnitTypeDefPtr angleTypeDef = stdLib->getUnitTypeDef("angle"))
        {
            unitRegistry->addUnitConverter(angleTypeDef,
                                           mx::LinearUnitConverter::create(angleTypeDef));
        }
        unitSystem->setUnitConverterRegistry(unitRegistry);
        generator->setUnitSystem(unitSystem);

        mx::GenContext context(generator);
        context.registerSourceCodeSearchPath(searchPath);
        context.getOptions().targetDistanceUnit = "meter";
        // GlslRenderer relies on this option to mark shaders that need alpha
        // blending.  MaterialXView performs the same transparency analysis
        // before generation.
        context.getOptions().hwTransparency =
            mx::isTransparentSurface(elem, generator->getTarget());
        // FIS (the default) generates a huge shader that takes seconds for the
        // GL driver to compile; the prefiltered environment method is what
        // MaterialXView uses by default and compiles far faster.
        context.getOptions().hwSpecularEnvironmentMethod = mx::SPECULAR_ENVIRONMENT_PREFILTER;

        // Offscreen renderer (hidden window + WGL context).
        mx::GlslRendererPtr renderer = mx::GlslRenderer::create(size, size);
        renderer->initialize();

        // Texture paths (including the document's fileprefix) are relative to
        // the material document's own directory, so add it to the search path.
        mx::FileSearchPath imageSearchPath = searchPath;
        imageSearchPath.append(mx::FilePath(mtlx_path).getParentPath());

        mx::ImageHandlerPtr imageHandler =
            mx::GLTextureHandler::create(mx::StbImageLoader::create());
        imageHandler->setSearchPath(imageSearchPath);
        renderer->setImageHandler(imageHandler);

        // Preview geometry: the MaterialX shader ball, scaled so its bounding
        // sphere fits the 45 degree camera view.
        mx::GeometryHandlerPtr geomHandler = renderer->getGeometryHandler();
        geomHandler->addLoader(mx::CgltfLoader::create());
        mx::FilePath geomPath = searchPath.find("resources/Geometry/shaderball.glb");
        if (!geomHandler->loadGeometry(geomPath))
        {
            set_error(err_buf, err_buf_len,
                      "failed to load preview geometry: " + geomPath.asString());
            return 1;
        }
        const mx::Vector3 boxMin = geomHandler->getMinimumBounds();
        const mx::Vector3 boxMax = geomHandler->getMaximumBounds();
        const mx::Vector3 center = (boxMax + boxMin) * 0.5f;
        const float radius = ((boxMax - boxMin) * 0.5f).getMagnitude();
        if (radius > 0.0f)
        {
            const float fit = 1.0f / radius;
            renderer->getCamera()->setWorldMatrix(
                mx::Matrix44::createTranslation(center * -1.0f) *
                mx::Matrix44::createScale(mx::Vector3(fit, fit, fit)));
        }

        // Use an elevated three-quarter view instead of the default head-on
        // camera.  Keep the eye roughly three units from the normalized ball
        // so its framing remains consistent with MaterialX's default view.
        renderer->getCamera()->setViewMatrix(mx::Camera::createViewMatrix(
            mx::Vector3(1.5f, 1.1f, 2.3f),
            mx::Vector3(0.0f, 0.0f, 0.0f),
            mx::Vector3(0.0f, 1.0f, 0.0f)));

        // Lighting: direct light rig + split environment maps, like the viewer.
        mx::LightHandlerPtr lightHandler = mx::LightHandler::create();
        mx::DocumentPtr lightRigDoc = mx::createDocument();
        mx::readFromXmlFile(lightRigDoc,
                            searchPath.find("resources/Lights/san_giuseppe_bridge_split.mtlx"));
        doc->importLibrary(lightRigDoc);
        std::vector<mx::NodePtr> lights;
        lightHandler->findLights(doc, lights);
        lightHandler->registerLights(doc, lights, context);
        lightHandler->setLightSources(lights);

        mx::ImagePtr envRadiance = imageHandler->acquireImage(
            searchPath.find("resources/Lights/san_giuseppe_bridge_split.hdr"));
        mx::ImagePtr envIrradiance = imageHandler->acquireImage(
            searchPath.find("resources/Lights/irradiance/san_giuseppe_bridge_split.hdr"));
        lightHandler->setEnvRadianceMap(envRadiance);
        lightHandler->setEnvIrradianceMap(envIrradiance);
        lightHandler->setEnvSampleCount(16);
        if (mx::elementRequiresShading(elem))
        {
            renderer->setLightHandler(lightHandler);
        }

        // Generate, compile, render.
        mx::ShaderPtr shader = generator->generate("thumbnail", elem, context);
        if (canonicalGltfPbr)
        {
            remove_public_uniform_initializers(shader);
        }
        renderer->createProgram(shader);
        renderer->validateInputs();
        renderer->setSize(size, size);
        mx::GeometryHandlerPtr envGeometry = mx::GeometryHandler::create();
        envGeometry->addLoader(mx::TinyObjLoader::create());
        mx::FilePath envSpherePath =
            searchPath.find("resources/Geometry/sphere.obj");
        if (!envGeometry->loadGeometry(envSpherePath))
        {
            set_error(err_buf, err_buf_len,
                        "failed to load environment geometry: " +
                            envSpherePath.asString());
            return 1;
        }

        mx::GlslMaterialPtr envMaterial = mx::GlslMaterial::create();
        mx::FilePath envMaterialPath =
            searchPath.find("resources/Lights/environment_map.mtlx");
        mx::FilePath envBackgroundPath =
            searchPath.find("resources/Lights/irradiance/san_giuseppe_bridge.hdr");
        if (!generate_environment_shader(envMaterial, context,
                                            envMaterialPath, stdLib,
                                            envBackgroundPath))
        {
            set_error(err_buf, err_buf_len,
                        "failed to generate environment background shader");
            return 1;
        }

        render_with_environment(renderer, envMaterial, envGeometry,
                                imageHandler, searchPath, geomHandler,
                                lightHandler);

        mx::ImagePtr image = renderer->captureImage();
        if (!image || image->getChannelCount() != 4 ||
            image->getBaseType() != mx::Image::BaseType::UINT8 ||
            image->getWidth() != size || image->getHeight() != size)
        {
            set_error(err_buf, err_buf_len, "unexpected captured image format");
            return 1;
        }

        // The GL readback is bottom-up; flip to top-down while copying out.
        const uint8_t* src = static_cast<const uint8_t*>(image->getResourceBuffer());
        const size_t rowBytes = static_cast<size_t>(size) * 4;
        for (uint32_t y = 0; y < size; ++y)
        {
            std::memcpy(out_rgba + static_cast<size_t>(y) * rowBytes,
                        src + static_cast<size_t>(size - 1 - y) * rowBytes, rowBytes);
        }
        return 0;
    }
    catch (mx::ExceptionRenderError& e)
    {
        std::string message = std::string("render error: ") + e.what();
        for (const std::string& line : e.errorLog())
        {
            message += "\n" + line;
        }
        set_error(err_buf, err_buf_len, message);
        return 1;
    }
    catch (std::exception& e)
    {
        set_error(err_buf, err_buf_len, e.what());
        return 1;
    }
}
