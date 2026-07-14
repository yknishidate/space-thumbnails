use std::{
    cell::Cell,
    ffi::OsString,
    fs, io,
    os::windows::prelude::OsStringExt,
    time::{Duration, Instant},
};

use lazy_static::lazy_static;
use log::{debug, warn};
use windows::{
    core::{implement, IUnknown, Interface, GUID},
    Win32::{
        Foundation::E_FAIL,
        Graphics::Gdi::HBITMAP,
        UI::Shell::{
            IThumbnailProvider_Impl, PropertiesSystem::IInitializeWithFile_Impl, WTSAT_ARGB,
            WTS_ALPHATYPE,
        },
    },
};

use crate::{
    constant::{ERROR_256X256_ARGB, TIMEOUT_256X256_ARGB, TOOLARGE_256X256_ARGB},
    helper_client::{find_helper, HelperClient, RenderResult},
    logging::{error_label, input_label},
    registry::{register_clsid, RegistryData, RegistryKey, RegistryValue},
    utils::create_argb_bitmap,
};

use super::Provider;

const THUMBNAIL_SIZE: u32 = 256;
const HELPER_EXE: &str = "space-thumbnails-render-helper.exe";
const HELPER_OVERRIDE_ENV: &str = "SPACE_THUMBNAILS_RENDER_HELPER";
/// The helper reuses its engine, but a cold start pays Filament init (~250ms)
/// plus load/render; 8s is generous while still bounding a runaway file.
const HELPER_TIMEOUT: Duration = Duration::from_secs(8);

lazy_static! {
    static ref RENDER_HELPER: Option<HelperClient> =
        find_helper(HELPER_EXE, HELPER_OVERRIDE_ENV).map(|path| HelperClient::new(path, HELPER_TIMEOUT));
}

/// Thumbnail provider for every Filament-backed model format.
///
/// It runs in-process (`IInitializeWithFile` handlers are only invoked in the
/// caller's process), but does no parsing or rendering itself — it forwards
/// the real file path to the persistent out-of-process render helper and
/// turns the returned pixels into a bitmap. All the crash-prone work is thus
/// isolated in the helper, and formats with external resources (e.g. `.gltf`
/// with sibling `.bin`/textures) resolve because the helper gets a real path.
pub struct ThumbnailFileProvider {
    pub clsid: GUID,
    pub file_extension: &'static str,
}

impl ThumbnailFileProvider {
    pub fn new(clsid: GUID, file_extension: &'static str) -> Self {
        Self {
            clsid,
            file_extension,
        }
    }
}

impl Provider for ThumbnailFileProvider {
    fn clsid(&self) -> windows::core::GUID {
        self.clsid
    }

    fn register(&self, module_path: &str) -> Vec<crate::registry::RegistryKey> {
        // DisableProcessIsolation=1: file handlers are only invoked in-process.
        // Safe here because the in-process work is just IPC to the helper.
        let mut result = register_clsid(&self.clsid(), module_path, true);
        result.push(RegistryKey {
            path: format!(
                "{}\\ShellEx\\{{{:?}}}",
                self.file_extension,
                windows::Win32::UI::Shell::IThumbnailProvider::IID
            ),
            values: vec![RegistryValue(
                "".to_owned(),
                RegistryData::Str(format!("{{{:?}}}", &self.clsid())),
            )],
        });
        result
    }

    fn create_instance(
        &self,
        riid: *const windows::core::GUID,
        ppv_object: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        ThumbnailFileHandler::new(riid, ppv_object)
    }
}

#[implement(
    windows::Win32::UI::Shell::IThumbnailProvider,
    windows::Win32::UI::Shell::PropertiesSystem::IInitializeWithFile
)]
pub struct ThumbnailFileHandler {
    filepath: Cell<String>,
}

impl ThumbnailFileHandler {
    pub fn new(
        riid: *const GUID,
        ppv_object: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        let unknown: IUnknown = ThumbnailFileHandler {
            filepath: Cell::new(String::new()),
        }
        .into();
        unsafe { unknown.query(&*riid, ppv_object).ok() }
    }
}

unsafe fn write_image(image: &[u8], phbmp: *mut HBITMAP, pdwalpha: *mut WTS_ALPHATYPE) {
    let mut p_bits: *mut core::ffi::c_void = core::ptr::null_mut();
    let hbmp = create_argb_bitmap(256, 256, &mut p_bits);
    std::ptr::copy(image.as_ptr(), p_bits as *mut _, image.len());
    phbmp.write(hbmp);
    pdwalpha.write(WTSAT_ARGB);
}

impl IThumbnailProvider_Impl for ThumbnailFileHandler {
    fn GetThumbnail(
        &self,
        _: u32,
        phbmp: *mut HBITMAP,
        pdwalpha: *mut WTS_ALPHATYPE,
    ) -> windows::core::Result<()> {
        let filepath = self.filepath.take();
        let size = THUMBNAIL_SIZE;

        if filepath.is_empty() {
            return Err(windows::core::Error::from(E_FAIL));
        }

        if matches!(fs::metadata(&filepath), Ok(metadata) if metadata.len() > 300 * 1024 * 1024 /* 300 MB */)
        {
            unsafe { write_image(TOOLARGE_256X256_ARGB, phbmp, pdwalpha) };
            return Ok(());
        }

        let start_time = Instant::now();
        let input = input_label(&filepath);

        let helper = match RENDER_HELPER.as_ref() {
            Some(helper) => helper,
            None => {
                warn!(target: "ThumbnailFileProvider", "render helper executable not found");
                unsafe { write_image(ERROR_256X256_ARGB, phbmp, pdwalpha) };
                return Ok(());
            }
        };

        match helper.render(&filepath, size) {
            Ok(RenderResult::Pixels(pixels)) if pixels.len() == (size * size * 4) as usize => {
                debug!(target: "ThumbnailFileProvider", "thumbnail completed: input={}, outcome=success, elapsed={:.2?}", input, start_time.elapsed());
                unsafe {
                    let mut p_bits: *mut core::ffi::c_void = core::ptr::null_mut();
                    let hbmp = create_argb_bitmap(size, size, &mut p_bits);
                    for i in 0..(size * size) as usize {
                        let r = pixels[i * 4];
                        let g = pixels[i * 4 + 1];
                        let b = pixels[i * 4 + 2];
                        let a = pixels[i * 4 + 3];
                        (p_bits as *mut u32).add(i).write(
                            (a as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32,
                        );
                    }
                    phbmp.write(hbmp);
                    pdwalpha.write(WTSAT_ARGB);
                }
                Ok(())
            }
            Ok(RenderResult::Pixels(_)) => {
                warn!(target: "ThumbnailFileProvider", "thumbnail failed: input={}, error=unexpected_pixel_count, elapsed={:.2?}", input, start_time.elapsed());
                unsafe { write_image(ERROR_256X256_ARGB, phbmp, pdwalpha) };
                Ok(())
            }
            // Valid file, nothing to draw (e.g. an Alembic locator-only scene):
            // show the neutral "no preview" image, not the broken-file one.
            Ok(RenderResult::Empty) => {
                debug!(target: "ThumbnailFileProvider", "thumbnail completed: input={}, outcome=no_geometry, elapsed={:.2?}", input, start_time.elapsed());
                unsafe { write_image(TIMEOUT_256X256_ARGB, phbmp, pdwalpha) };
                Ok(())
            }
            Err(err) if err.kind() == io::ErrorKind::TimedOut => {
                warn!(target: "ThumbnailFileProvider", "thumbnail timed out: input={}, elapsed={:.2?}", input, start_time.elapsed());
                unsafe { write_image(TIMEOUT_256X256_ARGB, phbmp, pdwalpha) };
                Ok(())
            }
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::NotFound
                        | io::ErrorKind::BrokenPipe
                        | io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::InvalidData
                ) =>
            {
                warn!(target: "ThumbnailFileProvider", "thumbnail infrastructure failure: input={}, error={:?}, elapsed={:.2?}", input, err.kind(), start_time.elapsed());
                unsafe { write_image(ERROR_256X256_ARGB, phbmp, pdwalpha) };
                Ok(())
            }
            Err(err) => {
                debug!(target: "ThumbnailFileProvider", "thumbnail failed: input={}, error={}, elapsed={:.2?}", input, error_label(&err), start_time.elapsed());
                unsafe { write_image(ERROR_256X256_ARGB, phbmp, pdwalpha) };
                Ok(())
            }
        }
    }
}

impl IInitializeWithFile_Impl for ThumbnailFileHandler {
    fn Initialize(
        &self,
        pszfilepath: &windows::core::PCWSTR,
        _grfmode: u32,
    ) -> windows::core::Result<()> {
        let filepath = unsafe {
            let str_p = pszfilepath.0;
            let mut str_len = 0;
            loop {
                if str_p.add(str_len).read() != 0 {
                    str_len += 1;
                    if str_len > 1024 {
                        return Err(E_FAIL.into());
                    }
                    continue;
                } else {
                    break;
                }
            }
            if str_len > 0 {
                OsString::from_wide(core::slice::from_raw_parts(str_p, str_len))
                    .to_str()
                    .map(|s| s.to_owned())
            } else {
                None
            }
        };
        if let Some(filepath) = filepath {
            self.filepath.set(filepath);
            Ok(())
        } else {
            Err(E_FAIL.into())
        }
    }
}
