//! Minimal safe wrapper over the static Alembic mesh-reading bridge.
//!
//! Reads the first time-sample of every polymesh in an `.abc` archive, merged
//! into one position/index buffer (world-space, triangulated). Normals, UVs,
//! materials and animation are dropped — callers render a plain gray mesh.

use std::{
    ffi::c_char,
    os::windows::ffi::OsStrExt,
    path::Path,
};

#[repr(C)]
struct AbcMesh {
    positions: *mut f32,
    vertex_count: u32,
    indices: *mut u32,
    index_count: u32,
}

extern "C" {
    fn abc_read_mesh(path: *const c_char, out: *mut AbcMesh, err: *mut c_char, err_len: u32)
        -> i32;
    fn abc_read_mesh_from_memory(
        data: *const u8,
        len: usize,
        out: *mut AbcMesh,
        err: *mut c_char,
        err_len: u32,
    ) -> i32;
    fn abc_free_mesh(mesh: *mut AbcMesh);
}

#[link(name = "kernel32")]
extern "system" {
    fn GetShortPathNameW(long: *const u16, short: *mut u16, short_len: u32) -> u32;
}

/// A triangulated mesh read from an Alembic archive.
pub struct AlembicMesh {
    /// Interleaved xyz positions, `vertex_count * 3` long.
    pub positions: Vec<f32>,
    /// Triangle indices into `positions` (by vertex, not float), CCW.
    pub indices: Vec<u32>,
}

impl AlembicMesh {
    pub fn vertex_count(&self) -> usize {
        self.positions.len() / 3
    }
}

/// Alembic opens files with narrow (ANSI) C++ streams, so non-ASCII paths must
/// be reduced to their 8.3 short form first (best effort). Returns bytes ready
/// to pass as a C string, or `None` if the path can't be made C-safe.
fn c_path(path: &Path) -> Option<Vec<u8>> {
    let ascii = path.to_str().filter(|s| s.is_ascii());
    if let Some(ascii) = ascii {
        return Some(ascii.bytes().chain([0]).collect());
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut short = vec![0u16; 1024];
    let len = unsafe { GetShortPathNameW(wide.as_ptr(), short.as_mut_ptr(), short.len() as u32) };
    if len == 0 || len as usize >= short.len() {
        return None;
    }
    short.truncate(len as usize);
    let short = String::from_utf16(&short).ok()?;
    short
        .is_ascii()
        .then(|| short.bytes().chain([0]).collect())
}

/// Reads an `.abc` file's geometry from a path. Returns `Err` with a message
/// on failure (missing file, no polymesh, corrupt archive) rather than
/// panicking.
pub fn read_mesh(path: &Path) -> Result<AlembicMesh, String> {
    let c_path = c_path(path).ok_or_else(|| "path is not representable for Alembic".to_owned())?;
    read_with(|mesh, err| unsafe {
        abc_read_mesh(
            c_path.as_ptr() as *const c_char,
            mesh,
            err.as_mut_ptr() as *mut c_char,
            err.len() as u32,
        )
    })
}

/// Reads Ogawa `.abc` geometry directly from in-memory bytes (no temp file).
/// Only Ogawa is supported for stream reading; HDF5 archives (not built here)
/// will report an error.
pub fn read_mesh_from_memory(bytes: &[u8]) -> Result<AlembicMesh, String> {
    read_with(|mesh, err| unsafe {
        abc_read_mesh_from_memory(
            bytes.as_ptr(),
            bytes.len(),
            mesh,
            err.as_mut_ptr() as *mut c_char,
            err.len() as u32,
        )
    })
}

/// Invokes a bridge reader, then copies the result out of the C-allocated
/// buffers and frees them.
fn read_with(read: impl FnOnce(*mut AbcMesh, &mut [u8]) -> i32) -> Result<AlembicMesh, String> {
    let mut mesh = AbcMesh {
        positions: std::ptr::null_mut(),
        vertex_count: 0,
        indices: std::ptr::null_mut(),
        index_count: 0,
    };
    let mut err = vec![0u8; 1024];
    let code = read(&mut mesh, &mut err);
    if code != 0 {
        let msg = err.split(|&b| b == 0).next().unwrap_or_default();
        return Err(String::from_utf8_lossy(msg).into_owned());
    }

    let positions =
        unsafe { std::slice::from_raw_parts(mesh.positions, mesh.vertex_count as usize * 3) }
            .to_vec();
    let indices =
        unsafe { std::slice::from_raw_parts(mesh.indices, mesh.index_count as usize) }.to_vec();
    unsafe { abc_free_mesh(&mut mesh) };

    Ok(AlembicMesh { positions, indices })
}
