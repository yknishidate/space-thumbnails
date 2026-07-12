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
#include <MaterialXGenShader/Shader.h>
#include <MaterialXGenShader/Util.h>
#include <MaterialXGenShader/DefaultColorManagementSystem.h>
#include <MaterialXGenShader/UnitSystem.h>
#include <MaterialXGenGlsl/GlslShaderGenerator.h>
#include <MaterialXRender/CgltfLoader.h>
#include <MaterialXRender/GeometryHandler.h>
#include <MaterialXRender/LightHandler.h>
#include <MaterialXRender/StbImageLoader.h>
#include <MaterialXRender/Util.h>
#include <MaterialXRenderGlsl/GlslRenderer.h>
#include <MaterialXRenderGlsl/GLTextureHandler.h>

#include <cstdint>
#include <cstring>
#include <string>

namespace mx = MaterialX;

namespace
{

void set_error(char* err_buf, uint32_t err_buf_len, const std::string& message)
{
    if (err_buf && err_buf_len > 0)
    {
        size_t count = message.size() < err_buf_len - 1 ? message.size() : err_buf_len - 1;
        std::memcpy(err_buf, message.c_str(), count);
        err_buf[count] = '\0';
    }
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
        mx::DocumentPtr stdLib = mx::createDocument();
        mx::loadLibraries({ "libraries" }, searchPath, stdLib);

        // Material document.
        mx::DocumentPtr doc = mx::createDocument();
        mx::readFromXmlFile(doc, mtlx_path, searchPath);
        doc->setDataLibrary(stdLib);

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
        // sphere fits the default camera (eye at (0, 0, 3), 45 degree FOV).
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
        renderer->createProgram(shader);
        renderer->validateInputs();
        renderer->setSize(size, size);
        renderer->render();

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
