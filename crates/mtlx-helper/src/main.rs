//! One-shot MaterialX thumbnail renderer.
//!
//! Renders a .mtlx material onto the MaterialX shader ball and writes the
//! result as PNG (when the output path ends in .png) or raw top-down RGBA8
//! bytes (any other path; consumed by the .mtlx thumbnail provider DLL).
//!
//! Runs as a separate process on purpose: MaterialX creates a GL context and
//! JIT-compiles generated GLSL, and a crash in that stack must never take
//! down the caller (explorer.exe).

use std::{
    env,
    ffi::OsStr,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    process::exit,
    time::Instant,
};

extern "C" {
    fn mtlx_render_thumbnail(
        mtlx_path: *const u8,
        data_root: *const u8,
        size: u32,
        out_rgba: *mut u8,
        err_buf: *mut u8,
        err_buf_len: u32,
    ) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetShortPathNameW(long: *const u16, short: *mut u16, short_len: u32) -> u32;
}

/// MaterialX opens files with narrow (ANSI) C++ streams, so non-ASCII paths
/// fail to resolve. Converting to the 8.3 short form yields an ASCII-safe
/// alias on volumes where short names are enabled (the default on C:).
/// Best-effort: returns the original path when conversion is unavailable.
fn to_ascii_safe_path(path: &Path) -> PathBuf {
    if path.as_os_str().to_str().map_or(false, |s| s.is_ascii()) {
        return path.to_owned();
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut short = vec![0u16; 1024];
    let len = unsafe { GetShortPathNameW(wide.as_ptr(), short.as_mut_ptr(), short.len() as u32) };
    if len == 0 || len as usize >= short.len() {
        return path.to_owned();
    }
    short.truncate(len as usize);
    PathBuf::from(std::ffi::OsString::from_wide(&short))
}

fn usage() -> ! {
    eprintln!(
        "usage: space-thumbnails-mtlx-helper --input <file.mtlx> --output <out.png|out.raw> \
         [--size N] [--data-root <dir>]"
    );
    exit(2);
}

fn main() {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut size: u32 = 256;
    let mut data_root: Option<PathBuf> = None;

    let mut args = env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--input") => input = args.next().map(PathBuf::from),
            Some("--output") => output = args.next().map(PathBuf::from),
            Some("--size") => {
                size = args
                    .next()
                    .and_then(|v| v.to_str().and_then(|s| s.parse().ok()))
                    .unwrap_or_else(|| usage())
            }
            Some("--data-root") => data_root = args.next().map(PathBuf::from),
            _ => usage(),
        }
    }
    let (Some(input), Some(output)) = (input, output) else {
        usage();
    };
    if size == 0 || size > 4096 {
        usage();
    }

    // Default data root: "MaterialX" next to the helper executable (the
    // installed layout); fall back to the source submodule for development.
    let data_root = data_root.unwrap_or_else(|| {
        let exe_dir = env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_owned));
        if let Some(installed) = exe_dir.map(|d| d.join("MaterialX")) {
            if installed.join("libraries").is_dir() {
                return installed;
            }
        }
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("MaterialX")
    });

    let input = to_ascii_safe_path(&input);
    let data_root = to_ascii_safe_path(&data_root);
    let (Some(input_utf8), Some(data_root_utf8)) = (input.to_str(), data_root.to_str()) else {
        eprintln!("error: paths could not be converted to an ASCII-safe form");
        exit(1);
    };

    let start = Instant::now();
    let mut pixels = vec![0u8; size as usize * size as usize * 4];
    let mut err_buf = vec![0u8; 4096];
    let result = unsafe {
        mtlx_render_thumbnail(
            format!("{}\0", input_utf8).as_ptr(),
            format!("{}\0", data_root_utf8).as_ptr(),
            size,
            pixels.as_mut_ptr(),
            err_buf.as_mut_ptr(),
            err_buf.len() as u32,
        )
    };
    if result != 0 {
        let message = err_buf.split(|&b| b == 0).next().unwrap_or_default();
        eprintln!("error: {}", String::from_utf8_lossy(message));
        exit(1);
    }
    eprintln!("[perf] mtlx render ({}px): {:.2?}", size, start.elapsed());

    let is_png = output.extension() == Some(OsStr::new("png"));
    let write_result = if is_png {
        image::save_buffer(
            &output,
            &pixels,
            size,
            size,
            image::ColorType::Rgba8,
        )
        .map_err(|e| e.to_string())
    } else {
        std::fs::write(&output, &pixels).map_err(|e| e.to_string())
    };
    if let Err(e) = write_result {
        eprintln!("error: failed to write {}: {}", output.display(), e);
        exit(1);
    }
}
