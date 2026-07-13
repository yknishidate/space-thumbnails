//! Generic client for a persistent, out-of-process rendering helper.
//!
//! Spawns the helper once (in `--server` mode), then exchanges framed
//! requests/responses over its stdin/stdout pipes, reusing the helper's
//! renderer/engine across thumbnails. Any failure (timeout, crash, broken
//! pipe) tears the helper down; the next request respawns it. All the
//! dangerous work (parsing, GPU rendering) lives in the helper process, so a
//! crash there never reaches the calling shell process.
//!
//! Framed protocol (little-endian):
//!   request:  [path_len: u32][size: u32][path: utf8]
//!   response: [status: i32][len: u32][payload]  (status 0 => bytes, else utf8 error)

use std::{
    ffi::OsString,
    io::{self, Read, Write},
    os::windows::ffi::OsStringExt,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{mpsc, Mutex},
    time::Duration,
};

use windows::Win32::{
    Foundation::HINSTANCE,
    System::LibraryLoader::{
        GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
        GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    },
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Directory of the module this code is linked into (the provider DLL). Used
/// to locate sibling helper executables in the install directory.
fn current_module_dir() -> Option<PathBuf> {
    unsafe {
        let mut module = HINSTANCE(0);
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
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

/// Locates a helper executable installed next to the provider DLL. An override
/// env var (if set and non-empty) wins, for development.
pub fn find_helper(exe_name: &str, override_env: &str) -> Option<PathBuf> {
    if let Some(overridden) = std::env::var_os(override_env) {
        if !overridden.is_empty() {
            return Some(PathBuf::from(overridden));
        }
    }
    let candidate = current_module_dir()?.join(exe_name);
    candidate.is_file().then_some(candidate)
}
/// Guards against a corrupt length field trying to allocate absurd buffers.
const MAX_RESPONSE_BYTES: usize = 4096 * 4096 * 4;

struct Response {
    status: i32,
    payload: Vec<u8>,
}

struct Worker {
    child: Child,
    input: ChildStdin,
    responses: mpsc::Receiver<io::Result<Response>>,
}

impl Worker {
    fn spawn(helper: &PathBuf) -> io::Result<Self> {
        let mut child = Command::new(helper)
            .arg("--server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
        let input = child.stdin.take().unwrap();
        let mut output = child.stdout.take().unwrap();
        let (sender, responses) = mpsc::channel();
        // A reader thread turns the blocking pipe into a channel so the caller
        // can apply a timeout with recv_timeout.
        std::thread::spawn(move || loop {
            let response = (|| {
                let mut header = [0u8; 8];
                output.read_exact(&mut header)?;
                let status = i32::from_le_bytes(header[..4].try_into().unwrap());
                let len = u32::from_le_bytes(header[4..].try_into().unwrap()) as usize;
                if len > MAX_RESPONSE_BYTES {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "oversized response"));
                }
                let mut payload = vec![0u8; len];
                output.read_exact(&mut payload)?;
                Ok(Response { status, payload })
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

    fn request(&mut self, path: &str, size: u32, timeout: Duration) -> io::Result<Response> {
        let path = path.as_bytes();
        self.input.write_all(&(path.len() as u32).to_le_bytes())?;
        self.input.write_all(&size.to_le_bytes())?;
        self.input.write_all(path)?;
        self.input.flush()?;

        self.responses.recv_timeout(timeout).map_err(|err| {
            let kind = if err == mpsc::RecvTimeoutError::Timeout {
                io::ErrorKind::TimedOut
            } else {
                io::ErrorKind::BrokenPipe
            };
            io::Error::new(kind, err.to_string())
        })?
    }

    fn terminate(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A persistent helper process plus the per-request timeout to apply. Cheap to
/// construct; the process is spawned lazily on first render.
pub struct HelperClient {
    helper: PathBuf,
    timeout: Duration,
    worker: Mutex<Option<Worker>>,
}

impl HelperClient {
    pub fn new(helper: PathBuf, timeout: Duration) -> Self {
        Self {
            helper,
            timeout,
            worker: Mutex::new(None),
        }
    }

    /// Renders `path` at `size`, returning the helper's raw byte payload
    /// (RGBA8 for the render helper). On any failure the helper is torn down so
    /// the next call starts fresh.
    pub fn render(&self, path: &str, size: u32) -> io::Result<Vec<u8>> {
        let mut slot = self
            .worker
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "helper lock poisoned"))?;
        if slot.is_none() {
            *slot = Some(Worker::spawn(&self.helper)?);
        }

        let result = (|| {
            let response = slot.as_mut().unwrap().request(path, size, self.timeout)?;
            if response.status != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    String::from_utf8_lossy(&response.payload).into_owned(),
                ));
            }
            Ok(response.payload)
        })();

        if result.is_err() {
            if let Some(worker) = slot.as_mut() {
                worker.terminate();
            }
            *slot = None;
        }
        result
    }
}
