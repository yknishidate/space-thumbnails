use core::panic;
use std::{cell::Cell, ffi::OsStr, fs, path::Path, rc::Rc, time::Instant};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use log::info;

const PERF_TARGET: &'static str = "SpaceThumbnailsPerf";

use filament_bindings::{
    assimp::{post_process, AssimpAsset},
    backend::{Backend, PixelBufferDescriptor, PixelDataFormat, PixelDataType},
    filament::{
        self, sRGBColor, Aabb, Camera, ClearOptions, Engine, IndirectLight, IndirectLightBuilder,
        LightBuilder, Renderer, Scene, SwapChain, SwapChainConfig, Texture, View, Viewport,
    },
    glftio::{
        AssetConfiguration, AssetLoader, MaterialProvider, ResourceConfiguration, ResourceLoader,
    },
    image::{ktx, KtxBundle},
    math::{Float3, Mat3f, Mat4f},
    utils::Entity,
};

const IDL_TEXTURE_DATA: &'static [u8] = include_bytes!("lightroom_14b_ibl.ktx");

const ASSIMP_FLAGS: u32 = post_process::GEN_SMOOTH_NORMALS
    | post_process::CALC_TANGENT_SPACE
    | post_process::GEN_UV_COORDS
    | post_process::FIND_INSTANCES
    | post_process::OPTIMIZE_MESHES
    | post_process::IMPROVE_CACHE_LOCALITY
    | post_process::SORT_BY_P_TYPE
    | post_process::TRIANGULATE;

pub struct SpaceThumbnailsRenderer {
    // need release
    engine: Engine,
    scene: Scene,
    ibl_texture: Texture,
    ibl: IndirectLight,
    swap_chain: SwapChain,
    renderer: Renderer,
    camera_entity: Entity,
    sunlight_entity: Entity,
    view: View,
    destroy_asset: Option<Box<dyn FnOnce(&mut Engine, &mut Scene)>>,

    viewport: Viewport,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum RendererBackend {
    Default = 0,
    OpenGL = 1,
    Vulkan = 2,
    Metal = 3,
}

impl SpaceThumbnailsRenderer {
    pub fn new(backend: RendererBackend, width: u32, height: u32) -> Self {
        let start = Instant::now();
        unsafe {
            let mut engine = Engine::create(match backend {
                RendererBackend::Default => Backend::DEFAULT,
                RendererBackend::OpenGL => Backend::OPENGL,
                RendererBackend::Vulkan => Backend::VULKAN,
                RendererBackend::Metal => Backend::METAL,
            })
            .unwrap();
            info!(target: PERF_TARGET, "engine create ({:?}): {:.2?}", backend, start.elapsed());
            let mut scene = engine.create_scene().unwrap();
            let mut swap_chain = engine
                .create_headless_swap_chain(width, height, SwapChainConfig::TRANSPARENT)
                .unwrap();
            let mut renderer = engine.create_renderer().unwrap();
            let mut view = engine.create_view().unwrap();
            let mut entity_manager = engine.get_entity_manager().unwrap();
            let camera_entity = entity_manager.create();
            let mut camera = engine.create_camera(&camera_entity).unwrap();
            let ibl_texture = ktx::create_texture(
                &mut engine,
                KtxBundle::from(IDL_TEXTURE_DATA).unwrap(),
                false,
            )
            .unwrap();

            let mut ibl = IndirectLightBuilder::new()
                .unwrap()
                .reflections(&ibl_texture)
                .intensity(50000.0)
                .rotation(&Mat3f::rotation(-90.0, Float3::new(0.0, 1.0, 0.0)))
                .build(&mut engine)
                .unwrap();
            scene.set_indirect_light(&mut ibl);

            let sunlight_entity = entity_manager.create();
            LightBuilder::new(filament::LightType::SUN)
                .unwrap()
                .color(&sRGBColor(Float3::new(0.98, 0.92, 0.89)).to_linear_fast())
                .intensity(100000.0)
                .direction(&Float3::new(0.6, -1.0, -0.8).normalize())
                .cast_shadows(true)
                .sun_angular_radius(1.0)
                .sun_halo_size(2.0)
                .sun_halo_falloff(80.0)
                .build(&mut engine, &sunlight_entity)
                .unwrap();

            scene.add_entity(&sunlight_entity);

            view.set_camera(&mut camera);
            view.set_scene(&mut scene);
            renderer.set_clear_options(&ClearOptions {
                clear_color: [0.0, 0.0, 0.0, 0.0].into(),
                clear: true,
                discard: false,
            });

            let viewport = Viewport {
                left: 0,
                bottom: 0,
                width,
                height,
            };

            view.set_viewport(&viewport);

            // warming up
            let warmup_start = Instant::now();
            renderer.begin_frame(&mut swap_chain);
            renderer.render(&mut view);
            renderer.end_frame();
            engine.flush_and_wait();
            info!(
                target: PERF_TARGET,
                "renderer init total ({}x{}): {:.2?} (warmup frame: {:.2?})",
                width,
                height,
                start.elapsed(),
                warmup_start.elapsed()
            );

            Self {
                engine,
                scene,
                ibl_texture,
                ibl,
                swap_chain,
                renderer,
                camera_entity,
                sunlight_entity,
                view,
                destroy_asset: None,
                viewport,
            }
        }
    }

    pub fn load_asset_from_file(&mut self, filepath: impl AsRef<Path>) -> Option<&mut Self> {
        if matches!(filepath.as_ref().extension(), Some(e) if e == "gltf" || e == "glb") {
            let start = Instant::now();
            let data = fs::read(&filepath).ok()?;
            info!(
                target: PERF_TARGET,
                "read file ({} bytes): {:.2?}",
                data.len(),
                start.elapsed()
            );
            self.load_gltf_asset(
                &data,
                filepath.as_ref().file_name()?,
                Some(filepath.as_ref()),
            )
        } else {
            let start = Instant::now();
            let asset =
                match AssimpAsset::from_file_with_flags(&mut self.engine, filepath, ASSIMP_FLAGS) {
                    Ok(asset) => asset,
                    Err(error) => {
                        log::warn!(target: PERF_TARGET, "assimp import failed: {error}");
                        return None;
                    }
                };
            info!(target: PERF_TARGET, "assimp import from file: {:.2?}", start.elapsed());
            self.load_assimp_asset(asset)
        }
    }

    pub fn load_asset_from_memory(
        &mut self,
        buffer: &[u8],
        filename: impl AsRef<OsStr>,
    ) -> Option<&mut Self> {
        if matches!(Path::new(filename.as_ref()).extension(), Some(e) if e == "gltf" || e == "glb")
        {
            self.load_gltf_asset(buffer, filename.as_ref(), None)
        } else {
            let start = Instant::now();
            let format_hint = Path::new(filename.as_ref())
                .extension()
                .and_then(OsStr::to_str)?;
            let asset = match AssimpAsset::from_memory_with_flags(
                &mut self.engine,
                buffer,
                format_hint,
                ASSIMP_FLAGS,
            ) {
                Ok(asset) => asset,
                Err(error) => {
                    log::warn!(target: PERF_TARGET, "assimp memory import failed ({format_hint}): {error}");
                    return None;
                }
            };
            info!(target: PERF_TARGET, "assimp import from memory: {:.2?}", start.elapsed());
            self.load_assimp_asset(asset)
        }
    }

    pub fn load_assimp_asset(&mut self, mut asset: AssimpAsset) -> Option<&mut Self> {
        let start = Instant::now();
        self.destroy_opened_asset();

        unsafe {
            let aabb = asset.get_aabb();
            info!(
                target: PERF_TARGET,
                "assimp bounds: min={:?}, max={:?}",
                aabb.min.vec,
                aabb.max.vec
            );
            let transform = fit_into_unit_cube(aabb);

            let mut transform_manager = self.engine.get_transform_manager()?;
            let root_entity = asset.get_root_entity();
            let root_transform_instance = transform_manager.get_instance(root_entity)?;
            transform_manager.set_transform_float(&root_transform_instance, &transform);

            self.scene.add_entities(asset.get_renderables());

            self.scene.add_entity(root_entity);

            let mut camera = self
                .engine
                .get_camera_component(&self.camera_entity)
                .unwrap();

            camera.set_exposure_physical(16.0, 1.0 / 125.0, 100.0);

            // Source files often contain authoring cameras that point away
            // from the model or frame only part of it. Thumbnails should
            // consistently frame the complete imported bounds instead.
            setup_camera_surround_view(&mut camera, &aabb.transform(transform), &self.viewport);

            self.destroy_asset = Some(Box::new(move |engine, scene| {
                scene.remove_entities(asset.get_renderables());
                scene.remove_entity(asset.get_root_entity());
                // note: "destory" is the (misspelled) method name in filament-bindings
                asset.destory(engine)
            }));
        }

        info!(target: PERF_TARGET, "scene setup (assimp): {:.2?}", start.elapsed());

        Some(self)
    }

    pub fn load_gltf_asset(
        &mut self,
        data: &[u8],
        filename: &OsStr,
        filepath: Option<&Path>,
    ) -> Option<&mut Self> {
        let start = Instant::now();
        self.destroy_opened_asset();

        let binary = matches!(Path::new(filename).extension(), Some(e) if e == "glb");

        // Current Filament gltfio handles EXT_meshopt_compression directly.
        let load_as_binary = binary;

        // morph targets crash the bundled filament version inside
        // ResourceLoader::loadResources (access violation), so strip them and
        // render the base mesh instead
        let sanitized_data;
        let data = match sanitize_gltf(data, load_as_binary) {
            SanitizedGltf::Stripped(clean) => {
                info!(
                    target: PERF_TARGET,
                    "gltf morph targets stripped ({} -> {} bytes)",
                    data.len(),
                    clean.len()
                );
                sanitized_data = clean;
                sanitized_data.as_slice()
            }
            SanitizedGltf::TooComplex { nodes, primitives } => {
                info!(
                    target: PERF_TARGET,
                    "gltf rejected, too complex for the bundled filament (nodes: {}, primitives: {})",
                    nodes,
                    primitives
                );
                return None;
            }
            SanitizedGltf::Unchanged => data,
        };

        let filepath_str = filepath.and_then(|p| p.to_str().map(|s| s.to_owned()));

        unsafe {
            let materials = MaterialProvider::create_ubershader_loader(&mut self.engine)?;
            let mut entity_manager = self.engine.get_entity_manager()?;
            let mut transform_manager = self.engine.get_transform_manager()?;
            let mut loader = AssetLoader::create(AssetConfiguration {
                engine: &mut self.engine,
                materials,
                entities: Some(&mut entity_manager),
                default_node_name: None,
            })?;

            let mut asset = if load_as_binary {
                loader.create_asset_from_binary(&data)?
            } else {
                loader.create_asset_from_json(&data)?
            };
            info!(target: PERF_TARGET, "gltf parse asset: {:.2?}", start.elapsed());

            let uris = asset.get_resource_uris();
            let has_external_resource = uris
                .map(|uris| uris.into_iter().any(|uri| !is_base64_data_uri(&uri)))
                .unwrap_or(false);
            info!(target: PERF_TARGET, "gltf checked resource uris (external: {})", has_external_resource);

            if filepath_str.is_none() && has_external_resource {
                return None;
            }

            let resources_start = Instant::now();
            let mut resource_loader = ResourceLoader::create(ResourceConfiguration {
                engine: &mut self.engine,
                gltf_path: filepath_str,
                normalize_skinning_weights: true,
                recompute_bounding_boxes: false,
                ignore_bind_transform: false,
            })
            .unwrap();
            info!(target: PERF_TARGET, "gltf resource loader created");
            resource_loader.load_resources(&mut asset);
            info!(target: PERF_TARGET, "gltf resources loaded");

            asset.release_source_data();
            info!(
                target: PERF_TARGET,
                "gltf load resources: {:.2?}",
                resources_start.elapsed()
            );

            let aabb = asset.get_bounding_box();
            let transform = fit_into_unit_cube(&aabb);
            let root_transform_instance = transform_manager.get_instance(&asset.get_root())?;

            transform_manager.set_transform_float(&root_transform_instance, &transform);

            self.scene.add_entities(asset.get_entities());

            let mut camera = self
                .engine
                .get_camera_component(&self.camera_entity)
                .unwrap();

            camera.set_exposure_physical(16.0, 1.0 / 125.0, 100.0);

            setup_camera_surround_view(&mut camera, &aabb.transform(transform), &self.viewport);

            self.destroy_asset = Some(Box::new(move |_engine, scene| {
                scene.remove_entities(asset.get_entities());
                loader.destroy_asset(&asset);
                loader.destroy_materials();
                core::mem::drop(loader);
            }));
        }

        info!(target: PERF_TARGET, "gltf load total: {:.2?}", start.elapsed());

        Some(self)
    }

    pub fn take_screenshot_sync(&mut self, output_memory: &mut [u8]) -> usize {
        let byte_count = self.get_screenshot_size_in_byte();

        if output_memory.len() < byte_count {
            panic!("Output memory space is not enough to take screenshot.")
        }

        unsafe {
            let ok: Rc<Cell<bool>> = Rc::new(Cell::new(false));
            let ok_inner = ok.clone();
            let pixel = PixelBufferDescriptor::from_raw_ptr_callback(
                output_memory.as_mut_ptr(),
                output_memory.len(),
                PixelDataFormat::RGBA,
                PixelDataType::UBYTE,
                move |_| ok_inner.set(true),
            );

            let start = Instant::now();
            self.renderer.begin_frame(&mut self.swap_chain);
            self.renderer.render(&mut self.view);
            self.renderer
                .read_pixels(0, 0, self.viewport.width, self.viewport.height, pixel);
            self.renderer.end_frame();
            let submit_elapsed = start.elapsed();
            self.engine.flush_and_wait();

            if ok.get() == false {
                panic!("Take screenshot failed");
            }

            info!(
                target: PERF_TARGET,
                "render + readback: {:.2?} (submit: {:.2?}, gpu wait: {:.2?})",
                start.elapsed(),
                submit_elapsed,
                start.elapsed() - submit_elapsed
            );
        }

        byte_count
    }

    pub fn get_size(&self) -> (u32, u32) {
        (self.viewport.width, self.viewport.height)
    }

    pub fn get_screenshot_size_in_byte(&self) -> usize {
        (self.viewport.width * self.viewport.height * 4) as usize
    }

    pub fn destroy_opened_asset(&mut self) -> &mut Self {
        let destroy_asset = self.destroy_asset.take();
        if let Some(destroy) = destroy_asset {
            destroy(&mut self.engine, &mut self.scene)
        }

        self
    }
}

impl Drop for SpaceThumbnailsRenderer {
    fn drop(&mut self) {
        unsafe {
            self.destroy_opened_asset();
            let mut entity_manager = self.engine.get_entity_manager().unwrap();
            self.engine.destroy_entity_components(&self.camera_entity);
            self.engine.destroy_entity_components(&self.sunlight_entity);
            // note: "destory" is the (misspelled) method name in filament-bindings
            entity_manager.destory(&mut self.camera_entity);
            entity_manager.destory(&mut self.sunlight_entity);
            self.engine.destroy_texture(&mut self.ibl_texture);
            self.engine.destroy_indirect_light(&mut self.ibl);
            self.engine.destroy_scene(&mut self.scene);
            self.engine.destroy_view(&mut self.view);
            self.engine.destroy_renderer(&mut self.renderer);
            self.engine.destroy_swap_chain(&mut self.swap_chain);
            Engine::destroy(&mut self.engine);
        }
    }
}

unsafe fn setup_camera_surround_view(camera: &mut Camera, aabb: &Aabb, viewport: &Viewport) {
    let aspect = viewport.width as f64 / viewport.height as f64;
    let half_extent = aabb.extent();
    camera.set_lens_projection(28.0, aspect, 0.01, f64::INFINITY);
    camera.look_at_up(
        &(aabb.center()
            + Float3::from(((half_extent[0] + half_extent[2]) / 2.0).max(half_extent[1]))
                * Float3::from([2.5, 1.7, 2.5])),
        &aabb.center(),
        &[0.0, 1.0, 0.0].into(),
    );
}

fn fit_into_unit_cube(bounds: &Aabb) -> Mat4f {
    let min = bounds.min;
    let max = bounds.max;
    let max_extent = f32::max(f32::max(max[0] - min[0], max[1] - min[1]), max[2] - min[2]);
    let scale_factor = 2.0 / max_extent;
    let center = (min + max) / 2.0;
    Mat4f::scaling(Float3::new(scale_factor, scale_factor, scale_factor))
        * Mat4f::translation(center * -1.0)
}

fn is_base64_data_uri(uri: &str) -> bool {
    uri.starts_with("data:") && uri.find(";base64,").is_some()
}

enum SanitizedGltf {
    /// Nothing to change, use the original bytes.
    Unchanged,
    /// Morph targets were stripped, use these bytes instead.
    Stripped(Vec<u8>),
    /// Refuse to render: the model exceeds the capacity of the bundled
    /// filament version (its handle arena is a compile-time constant and
    /// overflowing it crashes with an access violation).
    TooComplex { nodes: usize, primitives: usize },
}

/// The bundled filament crashes when its handle arena overflows
/// (NodePerformanceTest with 10k nodes reproduces this); real thumbnail
/// models stay far below these limits.
const MAX_GLTF_NODES: usize = 8192;
const MAX_GLTF_PRIMITIVES: usize = 4096;
const MAX_DECOMPRESSED_GLTF_BUFFER_BYTES: usize = 300 * 1024 * 1024;

fn sanitize_gltf(data: &[u8], binary: bool) -> SanitizedGltf {
    if binary {
        match extract_glb_json_chunk(data) {
            Some(json) => match sanitize_gltf_json(json) {
                SanitizedGltf::Stripped(sanitized_json) => {
                    match rebuild_glb(data, &sanitized_json) {
                        Some(glb) => SanitizedGltf::Stripped(glb),
                        None => SanitizedGltf::Unchanged,
                    }
                }
                other => other,
            },
            None => SanitizedGltf::Unchanged,
        }
    } else {
        sanitize_gltf_json(data)
    }
}

/// Checks model complexity and removes morph targets
/// (`meshes[].primitives[].targets`), the associated default weights and
/// `extras` (cgltf parses `targetNames` from there), and animations (they may
/// reference the removed targets) from a glTF JSON document.
fn sanitize_gltf_json(json_bytes: &[u8]) -> SanitizedGltf {
    let mut root: serde_json::Value = match serde_json::from_slice(json_bytes) {
        Ok(root) => root,
        // let the regular loader report the parse error
        Err(_) => return SanitizedGltf::Unchanged,
    };

    let nodes = root
        .get("nodes")
        .and_then(|n| n.as_array())
        .map(|n| n.len())
        .unwrap_or(0);
    let primitives = root
        .get("meshes")
        .and_then(|m| m.as_array())
        .map(|meshes| {
            meshes
                .iter()
                .filter_map(|mesh| mesh.get("primitives").and_then(|p| p.as_array()))
                .map(|p| p.len())
                .sum()
        })
        .unwrap_or(0);
    if nodes > MAX_GLTF_NODES || primitives > MAX_GLTF_PRIMITIVES {
        return SanitizedGltf::TooComplex { nodes, primitives };
    }

    let mut modified = false;

    if let Some(meshes) = root.get_mut("meshes").and_then(|m| m.as_array_mut()) {
        for mesh in meshes {
            let mut mesh_modified = false;
            if let Some(primitives) = mesh.get_mut("primitives").and_then(|p| p.as_array_mut()) {
                for primitive in primitives {
                    if let Some(obj) = primitive.as_object_mut() {
                        if obj.remove("targets").is_some() {
                            mesh_modified = true;
                        }
                    }
                }
            }
            if mesh_modified {
                if let Some(obj) = mesh.as_object_mut() {
                    obj.remove("weights");
                    obj.remove("extras");
                }
                modified = true;
            }
        }
    }

    if !modified {
        return SanitizedGltf::Unchanged;
    }

    if let Some(nodes) = root.get_mut("nodes").and_then(|n| n.as_array_mut()) {
        for node in nodes {
            if let Some(obj) = node.as_object_mut() {
                obj.remove("weights");
            }
        }
    }
    if let Some(obj) = root.as_object_mut() {
        obj.remove("animations");
    }

    match serde_json::to_vec(&root) {
        Ok(bytes) => SanitizedGltf::Stripped(bytes),
        Err(_) => SanitizedGltf::Unchanged,
    }
}

/// Returns the JSON chunk of a GLB container.
fn extract_glb_json_chunk(data: &[u8]) -> Option<&[u8]> {
    if data.len() < 20 || &data[0..4] != b"glTF" {
        return None;
    }
    let chunk_len = u32::from_le_bytes(data[12..16].try_into().ok()?) as usize;
    if &data[16..20] != b"JSON" {
        return None;
    }
    data.get(20..20 + chunk_len)
}

/// Returns the first BIN chunk of a GLB container.
fn extract_glb_bin_chunk(data: &[u8]) -> Option<&[u8]> {
    if data.len() < 20 || &data[0..4] != b"glTF" {
        return None;
    }

    let declared_len = u32::from_le_bytes(data[8..12].try_into().ok()?) as usize;
    let end = declared_len.min(data.len());
    let mut offset = 12usize;
    while offset.checked_add(8)? <= end {
        let chunk_len = u32::from_le_bytes(data[offset..offset + 4].try_into().ok()?) as usize;
        let chunk_type = &data[offset + 4..offset + 8];
        let chunk_start = offset.checked_add(8)?;
        let chunk_end = chunk_start.checked_add(chunk_len)?;
        let chunk = data.get(chunk_start..chunk_end)?;
        if chunk_type == b"BIN\0" {
            return Some(chunk);
        }
        offset = chunk_end;
    }
    None
}

/// Rebuilds a GLB container with a replacement JSON chunk, keeping all
/// following chunks (BIN, ...) as-is.
fn rebuild_glb(original: &[u8], new_json: &[u8]) -> Option<Vec<u8>> {
    let old_chunk_len = u32::from_le_bytes(original[12..16].try_into().ok()?) as usize;
    let rest = original.get(20 + old_chunk_len..)?;

    // the JSON chunk must be 4-byte aligned, padded with trailing spaces
    let padded_len = (new_json.len() + 3) & !3;
    let total_len = 12 + 8 + padded_len + rest.len();

    let mut glb = Vec::with_capacity(total_len);
    glb.extend_from_slice(&original[0..8]); // magic + version
    glb.extend_from_slice(&(total_len as u32).to_le_bytes());
    glb.extend_from_slice(&(padded_len as u32).to_le_bytes());
    glb.extend_from_slice(b"JSON");
    glb.extend_from_slice(new_json);
    glb.resize(glb.len() + (padded_len - new_json.len()), b' ');
    glb.extend_from_slice(rest);

    Some(glb)
}

#[cfg(test)]
mod test {
    use std::{fs, io::Cursor, path::PathBuf, str::FromStr, time::Instant};

    use image::{ImageBuffer, ImageOutputFormat, Rgba};

    use crate::{RendererBackend, SpaceThumbnailsRenderer};

    #[test]
    fn render_file_test() {
        let models = fs::read_dir(
            PathBuf::from_str(env!("CARGO_MANIFEST_DIR"))
                .unwrap()
                .join("models"),
        )
        .unwrap();

        dbg!(std::env::temp_dir());

        for entry in models {
            let entry = entry.unwrap();

            if entry.file_type().unwrap().is_dir() {
                continue;
            }

            let filepath = entry.path();
            let filename = filepath.file_name().unwrap().to_str().unwrap();

            let now = Instant::now();
            let mut renderer = SpaceThumbnailsRenderer::new(RendererBackend::Vulkan, 800, 800);
            let elapsed = now.elapsed();
            println!("Initialize renderer, Elapsed: {:.2?}", elapsed);

            let now = Instant::now();
            renderer.load_asset_from_file(&filepath).unwrap();
            let elapsed = now.elapsed();
            println!("Load model file {}, Elapsed: {:.2?}", filename, elapsed);

            let mut screenshot_buffer = vec![0; renderer.get_screenshot_size_in_byte()];

            let now = Instant::now();
            renderer.take_screenshot_sync(screenshot_buffer.as_mut_slice());
            let elapsed = now.elapsed();
            println!("Render and take screenshot, Elapsed: {:.2?}", elapsed);

            let image = ImageBuffer::<Rgba<u8>, _>::from_raw(800, 800, screenshot_buffer).unwrap();
            let mut encoded = Cursor::new(Vec::new());
            image
                .write_to(&mut encoded, ImageOutputFormat::Png)
                .unwrap();
            test_results::save!(
                format!(
                    "render_file_test/{}-screenshot.png",
                    filepath
                        .file_name()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .replace('.', "-")
                )
                .as_str(),
                encoded.get_ref().as_slice()
            )
        }
    }
}
