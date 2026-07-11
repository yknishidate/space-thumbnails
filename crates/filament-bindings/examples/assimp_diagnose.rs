use std::{
    env,
    ffi::{CStr, CString},
    fs,
    path::Path,
    process,
};

use filament_bindings::assimp::post_process;

fn main() {
    let path_arg = env::args_os().nth(1).unwrap_or_else(|| {
        eprintln!("usage: assimp_diagnose <model-file>");
        process::exit(2);
    });
    let path_ref = Path::new(&path_arg);
    let bytes = fs::read(path_ref).expect("failed to read model for memory import");
    let hint = CString::new(
        path_ref
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default(),
    )
    .unwrap();
    let path = path_ref
        .to_str()
        .and_then(|path| CString::new(path).ok())
        .unwrap_or_else(|| {
            eprintln!("model path is not valid UTF-8 or contains a null byte");
            process::exit(2);
        });

    let basic = post_process::TRIANGULATE;
    let geometry = basic
        | post_process::GEN_SMOOTH_NORMALS
        | post_process::CALC_TANGENT_SPACE
        | post_process::GEN_UV_COORDS;
    let cases = [
        ("none", 0),
        ("triangulate", basic),
        ("geometry", geometry),
        ("+ sort", geometry | post_process::SORT_BY_P_TYPE),
        ("+ instances", geometry | post_process::FIND_INSTANCES),
        ("+ optimize", geometry | post_process::OPTIMIZE_MESHES),
        (
            "full",
            geometry
                | post_process::FIND_INSTANCES
                | post_process::OPTIMIZE_MESHES
                | post_process::IMPROVE_CACHE_LOCALITY
                | post_process::SORT_BY_P_TYPE,
        ),
    ];

    for (name, flags) in cases {
        unsafe {
            let scene = russimp_sys::aiImportFile(path.as_ptr(), flags);
            if scene.is_null() {
                let error = CStr::from_ptr(russimp_sys::aiGetErrorString()).to_string_lossy();
                println!("{name:>12}: import failed: {error}");
                continue;
            }

            let scene_ref = &*scene;
            let mut vertices = 0usize;
            let mut faces = 0usize;
            for index in 0..scene_ref.mNumMeshes as usize {
                let mesh = &**scene_ref.mMeshes.add(index);
                vertices += mesh.mNumVertices as usize;
                faces += mesh.mNumFaces as usize;
            }
            println!(
                "{name:>12}: meshes={}, vertices={vertices}, faces={faces}",
                scene_ref.mNumMeshes
            );
            russimp_sys::aiReleaseImport(scene);
        }
    }

    unsafe {
        let scene = russimp_sys::aiImportFileFromMemory(
            bytes.as_ptr().cast(),
            bytes.len() as u32,
            geometry,
            hint.as_ptr(),
        );
        if scene.is_null() {
            let error = CStr::from_ptr(russimp_sys::aiGetErrorString()).to_string_lossy();
            println!("      memory: import failed: {error}");
        } else {
            let scene_ref = &*scene;
            let mut vertices = 0usize;
            let mut faces = 0usize;
            for index in 0..scene_ref.mNumMeshes as usize {
                let mesh = &**scene_ref.mMeshes.add(index);
                vertices += mesh.mNumVertices as usize;
                faces += mesh.mNumFaces as usize;
            }
            println!(
                "      memory: meshes={}, vertices={vertices}, faces={faces}, hint={}",
                scene_ref.mNumMeshes,
                hint.to_string_lossy()
            );
            russimp_sys::aiReleaseImport(scene);
        }
    }
}
