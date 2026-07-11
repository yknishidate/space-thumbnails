use std::{ffi::CString, ptr};

use crate::{backend::BufferDescriptor, bindgen, filament::Engine};

use super::GltfAsset;

pub struct ResourceConfiguration<'a> {
    pub engine: &'a mut Engine,

    pub gltf_path: Option<String>,

    pub normalize_skinning_weights: bool,

    // Retained in the Rust API for source compatibility. Filament 1.73 no
    // longer exposes these switches in ResourceConfiguration.
    pub recompute_bounding_boxes: bool,

    pub ignore_bind_transform: bool,
}

pub struct ResourceLoader {
    native: ptr::NonNull<bindgen::filament_gltfio_ResourceLoader>,
    stb_provider: Option<ptr::NonNull<bindgen::filament_gltfio_TextureProvider>>,
    ktx2_provider: Option<ptr::NonNull<bindgen::filament_gltfio_TextureProvider>>,
    webp_provider: Option<ptr::NonNull<bindgen::filament_gltfio_TextureProvider>>,
}

impl ResourceLoader {
    #[inline]
    pub fn native(&self) -> *const bindgen::filament_gltfio_ResourceLoader {
        self.native.as_ptr()
    }

    #[inline]
    pub fn native_mut(&mut self) -> *mut bindgen::filament_gltfio_ResourceLoader {
        self.native.as_ptr()
    }

    #[inline]
    pub fn try_from_native(native: *mut bindgen::filament_gltfio_ResourceLoader) -> Option<Self> {
        let ptr = ptr::NonNull::new(native)?;
        Some(ResourceLoader {
            native: ptr,
            stb_provider: None,
            ktx2_provider: None,
            webp_provider: None,
        })
    }

    pub unsafe fn create(config: ResourceConfiguration) -> Option<Self> {
        let native_gltf_path = if let Some(gltf_path) = config.gltf_path {
            CString::new(gltf_path).ok()
        } else {
            None
        };

        let native_config = bindgen::filament_gltfio_ResourceConfiguration {
            engine: config.engine.native_mut(),
            gltfPath: native_gltf_path
                .as_ref()
                .map(|s| s.as_ptr() as *mut _)
                .unwrap_or(core::ptr::null_mut()),
            normalizeSkinningWeights: config.normalize_skinning_weights,
        };

        let mut loader = ResourceLoader::try_from_native(bindgen::helper_gltfio_resource_loader_create(
            &native_config,
        ))?;

        loader.stb_provider = ptr::NonNull::new(bindgen::helper_gltfio_create_stb_provider(
            config.engine.native_mut(),
        ));
        loader.ktx2_provider = ptr::NonNull::new(bindgen::helper_gltfio_create_ktx2_provider(
            config.engine.native_mut(),
        ));
        loader.webp_provider = ptr::NonNull::new(bindgen::helper_gltfio_create_webp_provider(
            config.engine.native_mut(),
        ));
        if let Some(provider) = loader.stb_provider {
            for mime in [b"image/png\0".as_ptr(), b"image/jpeg\0".as_ptr()] {
                bindgen::filament_gltfio_ResourceLoader_addTextureProvider(
                    loader.native_mut(), mime.cast(), provider.as_ptr(),
                );
            }
        }
        if let Some(provider) = loader.ktx2_provider {
            bindgen::filament_gltfio_ResourceLoader_addTextureProvider(
                loader.native_mut(), b"image/ktx2\0".as_ptr().cast(), provider.as_ptr(),
            );
        }
        if let Some(provider) = loader.webp_provider {
            bindgen::filament_gltfio_ResourceLoader_addTextureProvider(
                loader.native_mut(), b"image/webp\0".as_ptr().cast(), provider.as_ptr(),
            );
        }
        Some(loader)
    }

    #[inline]
    pub unsafe fn add_resource_data(
        &mut self,
        uri: &str,
        buffer: BufferDescriptor<u8>,
    ) -> Result<(), std::ffi::NulError> {
        let c_uri = CString::new(uri)?;

        bindgen::filament_gltfio_ResourceLoader_addResourceData(
            self.native_mut(),
            c_uri.as_ptr(),
            &mut buffer.into_native(),
        );

        Ok(())
    }

    #[inline]
    pub unsafe fn has_resource_data(&self, uri: &str) -> Result<bool, std::ffi::NulError> {
        let c_uri = CString::new(uri)?;

        Ok(bindgen::filament_gltfio_ResourceLoader_hasResourceData(
            self.native(),
            c_uri.as_ptr(),
        ))
    }

    #[inline]
    pub unsafe fn evict_resource_data(&mut self) {
        bindgen::filament_gltfio_ResourceLoader_evictResourceData(self.native_mut())
    }

    #[inline]
    pub unsafe fn load_resources(&mut self, asset: &mut GltfAsset) -> bool {
        bindgen::filament_gltfio_ResourceLoader_loadResources(self.native_mut(), asset.native_mut())
    }

    // TODO: asyncBeginLoad
    // TODO: asyncGetLoadProgress
    // TODO: asyncUpdateLoad
    // TODO: asyncCancelLoad
}

impl Drop for ResourceLoader {
    fn drop(&mut self) {
        unsafe {
            bindgen::helper_gltfio_resource_loader_delete(self.native_mut());
            if let Some(provider) = self.stb_provider.take() {
                bindgen::helper_gltfio_texture_provider_delete(provider.as_ptr());
            }
            if let Some(provider) = self.ktx2_provider.take() {
                bindgen::helper_gltfio_texture_provider_delete(provider.as_ptr());
            }
            if let Some(provider) = self.webp_provider.take() {
                bindgen::helper_gltfio_texture_provider_delete(provider.as_ptr());
            }
        }
    }
}
