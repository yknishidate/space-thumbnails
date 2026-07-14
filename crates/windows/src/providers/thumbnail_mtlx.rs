use std::{
    cell::Cell,
    env,
    ffi::OsString,
    io::{self, Read, Write},
    os::windows::prelude::OsStringExt,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{mpsc, Mutex},
    time::{Duration, Instant},
};

use lazy_static::lazy_static;
use log::{debug, warn};
use windows::{
    core::{implement, IUnknown, Interface, GUID},
    Win32::{
        Foundation::{E_FAIL, HINSTANCE},
        Graphics::Gdi::HBITMAP,
        System::LibraryLoader::{
            GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
            GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        },
        UI::Shell::{
            IThumbnailProvider_Impl, PropertiesSystem::IInitializeWithFile_Impl, WTSAT_ARGB,
            WTS_ALPHATYPE,
        },
    },
};

use crate::{
    constant::{ERROR_256X256_ARGB, TIMEOUT_256X256_ARGB},
    logging::{error_label, input_label},
    registry::{register_clsid, RegistryData, RegistryKey, RegistryValue},
    utils::create_argb_bitmap,
};

use super::Provider;

const HELPER_EXE_NAME: &str = "space-thumbnails-mtlx-helper.exe";
const THUMBNAIL_SIZE: u32 = 256;
/// Generous: the helper JIT-compiles generated GLSL (~1-2s) and pays process
/// cold-start on top; runaway renders are killed at this bound.
const HELPER_TIMEOUT: Duration = Duration::from_secs(15);

/// Thumbnail provider for MaterialX (.mtlx) material documents.
///
/// Unlike the model providers this one does not render in-process: it keeps a
/// bundled helper process (statically linked against MaterialX) and exchanges
/// framed requests and raw pixels over pipes. The provider itself needs the real file path (for
/// `fileprefix`-relative texture resolution), hence `IInitializeWithFile` +
/// `DisableProcessIsolation`, which makes it run inside the calling process
/// (explorer.exe) — the helper-process split keeps GL/MaterialX crashes from
/// ever reaching the shell.
pub struct MtlxThumbnailProvider {
    pub clsid: GUID,
    pub file_extension: &'static str,
}

impl MtlxThumbnailProvider {
    pub const fn new(clsid: GUID, file_extension: &'static str) -> Self {
        Self {
            clsid,
            file_extension,
        }
    }
}

impl Provider for MtlxThumbnailProvider {
    fn clsid(&self) -> GUID {
        self.clsid
    }

    fn register(&self, module_path: &str) -> Vec<RegistryKey> {
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
        riid: *const GUID,
        ppv_object: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        let unknown: IUnknown = MtlxThumbnailHandler {
            filepath: Cell::new(String::new()),
        }
        .into();
        unsafe { unknown.query(&*riid, ppv_object).ok() }
    }
}

/// Directory containing the module this code is linked into (the provider
/// DLL), used to locate the helper exe and its data directory.
fn current_module_dir() -> Option<PathBuf> {
    unsafe {
        let mut module = HINSTANCE(0);
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            windows::core::PCWSTR(current_module_dir as *const u16),
            &mut module,
        )
        .ok()
        .ok()?;
        let mut path = vec![0u16; 1024];
        let len = GetModuleFileNameW(module, path.as_mut_slice()) as usize;
        if len == 0 || len >= path.len() {
            return None;
        }
        path.truncate(len);
        PathBuf::from(OsString::from_wide(&path))
            .parent()
            .map(Path::to_owned)
    }
}

fn find_helper() -> Option<PathBuf> {
    if let Some(overridden) = env::var_os("SPACE_THUMBNAILS_MTLX_HELPER") {
        return Some(PathBuf::from(overridden));
    }
    let candidate = current_module_dir()?.join(HELPER_EXE_NAME);
    candidate.is_file().then_some(candidate)
}

struct WorkerResponse {
    status: i32,
    payload: Vec<u8>,
}

struct MtlxWorker {
    child: Child,
    input: ChildStdin,
    responses: mpsc::Receiver<io::Result<WorkerResponse>>,
}

impl MtlxWorker {
    fn spawn(helper: &PathBuf) -> io::Result<Self> {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut child = Command::new(helper)
            .arg("--server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
        let input = child.stdin.take().unwrap();
        let mut output = child.stdout.take().unwrap();
        let (sender, responses) = mpsc::channel();
        std::thread::spawn(move || loop {
            let response = (|| {
                let mut header = [0u8; 8];
                output.read_exact(&mut header)?;
                let status = i32::from_le_bytes(header[..4].try_into().unwrap());
                let len = u32::from_le_bytes(header[4..].try_into().unwrap()) as usize;
                if len > 4096 * 4096 * 4 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "oversized response",
                    ));
                }
                let mut payload = vec![0u8; len];
                output.read_exact(&mut payload)?;
                Ok(WorkerResponse { status, payload })
            })();
            let failed = response.is_err();
            if sender.send(response).is_err() || failed {
                break;
            }
        });
        Ok(Self {
            child,
            input,
            responses,
        })
    }

    fn render(&mut self, filepath: &str) -> io::Result<Vec<u8>> {
        let path = filepath.as_bytes();
        self.input.write_all(&(path.len() as u32).to_le_bytes())?;
        self.input.write_all(&THUMBNAIL_SIZE.to_le_bytes())?;
        self.input.write_all(path)?;
        self.input.flush()?;

        let response = self
            .responses
            .recv_timeout(HELPER_TIMEOUT)
            .map_err(|err| {
                let kind = if err == mpsc::RecvTimeoutError::Timeout {
                    io::ErrorKind::TimedOut
                } else {
                    io::ErrorKind::BrokenPipe
                };
                io::Error::new(kind, err.to_string())
            })??;
        if response.status != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                String::from_utf8_lossy(&response.payload).into_owned(),
            ));
        }
        if response.payload.len() != (THUMBNAIL_SIZE * THUMBNAIL_SIZE * 4) as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("helper returned {} bytes", response.payload.len()),
            ));
        }
        Ok(response.payload)
    }

    fn terminate(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

lazy_static! {
    static ref MTLX_WORKER: Mutex<Option<MtlxWorker>> = Mutex::new(None);
}

fn render_via_helper(filepath: &str) -> io::Result<Vec<u8>> {
    let helper = find_helper().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "mtlx helper executable not found")
    })?;

    let mut slot = MTLX_WORKER
        .lock()
        .map_err(|_| io::Error::new(io::ErrorKind::Other, "worker lock poisoned"))?;
    if slot.is_none() {
        *slot = Some(MtlxWorker::spawn(&helper)?);
    }
    let result = slot.as_mut().unwrap().render(filepath);
    if result.is_err() {
        if let Some(worker) = slot.as_mut() {
            worker.terminate();
        }
        *slot = None;
    }
    result
}

unsafe fn write_static_image(
    image: &[u8],
    phbmp: *mut HBITMAP,
    pdwalpha: *mut WTS_ALPHATYPE,
) {
    let mut p_bits: *mut core::ffi::c_void = core::ptr::null_mut();
    let hbmp = create_argb_bitmap(256, 256, &mut p_bits);
    std::ptr::copy(image.as_ptr(), p_bits as *mut _, image.len());
    phbmp.write(hbmp);
    pdwalpha.write(WTSAT_ARGB);
}

#[implement(
    windows::Win32::UI::Shell::IThumbnailProvider,
    windows::Win32::UI::Shell::PropertiesSystem::IInitializeWithFile
)]
pub struct MtlxThumbnailHandler {
    filepath: Cell<String>,
}

impl IThumbnailProvider_Impl for MtlxThumbnailHandler {
    fn GetThumbnail(
        &self,
        _: u32,
        phbmp: *mut HBITMAP,
        pdwalpha: *mut WTS_ALPHATYPE,
    ) -> windows::core::Result<()> {
        let filepath = self.filepath.take();
        if filepath.is_empty() {
            return Err(windows::core::Error::from(E_FAIL));
        }

        let start_time = Instant::now();
        let input = input_label(&filepath);

        match render_via_helper(&filepath) {
            Ok(pixels) => {
                debug!(target: "MtlxThumbnailProvider", "thumbnail completed: input={}, outcome=success, elapsed={:.2?}", input, start_time.elapsed());
                let size = THUMBNAIL_SIZE;
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
            Err(err) if err.kind() == io::ErrorKind::TimedOut => {
                warn!(target: "MtlxThumbnailProvider", "thumbnail timed out: input={}, elapsed={:.2?}", input, start_time.elapsed());
                unsafe { write_static_image(TIMEOUT_256X256_ARGB, phbmp, pdwalpha) };
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
                warn!(target: "MtlxThumbnailProvider", "thumbnail infrastructure failure: input={}, error={:?}, elapsed={:.2?}", input, err.kind(), start_time.elapsed());
                unsafe { write_static_image(ERROR_256X256_ARGB, phbmp, pdwalpha) };
                Ok(())
            }
            Err(err) => {
                debug!(target: "MtlxThumbnailProvider", "thumbnail failed: input={}, error={}, elapsed={:.2?}", input, error_label(&err), start_time.elapsed());
                unsafe { write_static_image(ERROR_256X256_ARGB, phbmp, pdwalpha) };
                Ok(())
            }
        }
    }
}

impl IInitializeWithFile_Impl for MtlxThumbnailHandler {
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
