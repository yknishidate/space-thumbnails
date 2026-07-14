//! Isolated renderer process for all Filament-backed thumbnail formats
//! (obj/fbx/stl/dae/ply/x3d/3ds/gltf/glb/vrm/abc).
//!
//! The shell provider DLL is a thin in-process shim that forwards the file
//! path here over a pipe; all parsing and GPU rendering happen in this
//! separate process, so a crash in Filament/Assimp/Alembic on a malformed
//! file can never take down explorer.exe. Because the helper receives a real
//! path (not a stream), formats with external resources — notably `.gltf`
//! referencing sibling `.bin`/textures — resolve correctly.
//!
//! Server mode (`--server`) keeps the process alive across requests and
//! reuses the Filament engine (keyed by size), the same way the old
//! in-process render worker did.
//!
//! Framed protocol (little-endian), matching the MaterialX helper:
//!   request:  [path_len: u32][size: u32][path: utf8]
//!   response: [status: i32][len: u32][payload]
//!     status 0 => RGBA8 pixels; 2 => parsed but no renderable geometry (empty
//!     payload); anything else => utf8 error message.

use std::{
    collections::HashMap,
    env,
    fs::File,
    io::{self, Read, Write},
    os::windows::io::{FromRawHandle, RawHandle},
    path::Path,
    process::exit,
};

use space_thumbnails::{LoadError, RendererBackend, SpaceThumbnailsRenderer};

mod vrm_thumbnail;

const SERVER_ARG: &str = "--server";
const MAX_SIZE: u32 = 4096;

/// A successful render: either pixels, or a valid file with nothing to draw.
enum Rendered {
    Pixels(Vec<u8>),
    Empty,
}

extern "C" {
    fn _dup(fd: i32) -> i32;
    fn _get_osfhandle(fd: i32) -> isize;
    fn freopen(
        path: *const u8,
        mode: *const u8,
        stream: *mut core::ffi::c_void,
    ) -> *mut core::ffi::c_void;
    fn __acrt_iob_func(index: u32) -> *mut core::ffi::c_void;
}

/// Filament writes its startup banner to stdout (fd 1), which would corrupt
/// the binary response protocol. Duplicate the original stdout (the client
/// pipe) for exclusive protocol use, then point the CRT's stdout — the one
/// Filament's C++ streams write through — at NUL so its output is discarded.
/// Returns the clean protocol writer.
fn take_clean_output() -> File {
    unsafe {
        let saved = _dup(1);
        freopen(b"NUL\0".as_ptr(), b"w\0".as_ptr(), __acrt_iob_func(1));
        File::from_raw_handle(_get_osfhandle(saved) as RawHandle)
    }
}

/// Renders `path` at `size` using a cached renderer, returning top-down RGBA8.
/// A file that parses but holds no geometry yields `Rendered::Empty` (not an
/// error) so the caller can show a neutral "no preview" thumbnail.
fn render(
    renderers: &mut HashMap<u32, SpaceThumbnailsRenderer>,
    path: &str,
    size: u32,
) -> Result<Rendered, String> {
    if let Some(pixels) = vrm_thumbnail::load(Path::new(path), size) {
        return Ok(Rendered::Pixels(pixels));
    }

    let renderer = renderers
        .entry(size)
        .or_insert_with(|| SpaceThumbnailsRenderer::new(RendererBackend::Vulkan, size, size));

    match renderer.load_asset_from_file(path) {
        Ok(_) => {}
        // Both mean "valid file, nothing to show" — the caller draws the
        // neutral "no preview" thumbnail rather than a broken-file one.
        Err(LoadError::NoGeometry) | Err(LoadError::TooComplex) => return Ok(Rendered::Empty),
        Err(LoadError::Failed) => return Err(format!("failed to load {}", path)),
    }
    let mut buffer = vec![0u8; renderer.get_screenshot_size_in_byte()];
    renderer.take_screenshot_sync(buffer.as_mut_slice());
    // Free the asset so large scenes don't linger between requests.
    renderer.destroy_opened_asset();
    Ok(Rendered::Pixels(buffer))
}

fn run_server() -> io::Result<()> {
    // Must run before any renderer is created (which triggers Filament's
    // stdout banner).
    let mut output = take_clean_output();
    let mut input = io::stdin().lock();
    let mut renderers: HashMap<u32, SpaceThumbnailsRenderer> = HashMap::new();

    loop {
        let mut header = [0u8; 8];
        match input.read_exact(&mut header) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(err) => return Err(err),
        }
        let path_len = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
        let size = u32::from_le_bytes(header[4..].try_into().unwrap());
        if path_len > 1024 * 1024 || size == 0 || size > MAX_SIZE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid request"));
        }
        let mut path = vec![0u8; path_len];
        input.read_exact(&mut path)?;

        let response = match String::from_utf8(path) {
            Ok(path) => render(&mut renderers, &path, size),
            Err(err) => Err(err.to_string()),
        };
        let (status, payload) = match response {
            Ok(Rendered::Pixels(pixels)) => (0i32, pixels),
            Ok(Rendered::Empty) => (2i32, Vec::new()),
            Err(err) => (1i32, err.into_bytes()),
        };
        output.write_all(&status.to_le_bytes())?;
        output.write_all(&(payload.len() as u32).to_le_bytes())?;
        output.write_all(&payload)?;
        output.flush()?;
    }
}

fn main() {
    let mut args = env::args_os().skip(1);
    let first = args.next();
    if first.as_deref().and_then(|s| s.to_str()) == Some(SERVER_ARG) {
        if let Err(err) = run_server() {
            eprintln!("server error: {}", err);
            exit(1);
        }
        return;
    }

    // One-shot mode for manual testing: <input> <output.raw> [size].
    let input = first;
    let output = args.next();
    let size: u32 = args
        .next()
        .and_then(|s| s.to_str().and_then(|s| s.parse().ok()))
        .unwrap_or(256);
    let (Some(input), Some(output)) = (input, output) else {
        eprintln!("usage: space-thumbnails-render-helper (--server | <input> <output.raw> [size])");
        exit(2);
    };

    let mut renderers = HashMap::new();
    let input = input.to_string_lossy().into_owned();
    match render(&mut renderers, &input, size) {
        Ok(Rendered::Pixels(pixels)) => {
            if let Err(err) = std::fs::write(Path::new(&output), &pixels) {
                eprintln!("error: failed to write output: {}", err);
                exit(1);
            }
        }
        Ok(Rendered::Empty) => {
            eprintln!("no renderable geometry in {}", input);
            exit(1);
        }
        Err(err) => {
            eprintln!("error: {}", err);
            exit(1);
        }
    }
}
