use std::{
    env,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

fn run_command(command: &mut Command, program: &str) {
    let status = command
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .unwrap_or_else(|err| panic!("failed to run {}: {}", program, err));
    if !status.success() {
        panic!("{} exited with {}", program, status);
    }
}

/// Configure + build + install one cmake project into `install_dir`.
fn cmake_install(source: &Path, build: &Path, install_dir: &Path, args: &[String]) {
    std::fs::create_dir_all(build).unwrap();
    let mut configure = Command::new("cmake");
    configure
        .arg("-S")
        .arg(source)
        .arg("-B")
        .arg(build)
        .args(["-A", "x64"])
        .arg("-DCMAKE_BUILD_TYPE=Release")
        .arg(format!(
            "-DCMAKE_INSTALL_PREFIX={}",
            install_dir.to_str().unwrap()
        ))
        .arg(format!("-DCMAKE_PREFIX_PATH={}", install_dir.to_str().unwrap()));
    for arg in args {
        configure.arg(arg);
    }
    run_command(&mut configure, "cmake (configure)");

    let mut build_cmd = Command::new("cmake");
    build_cmd
        .arg("--build")
        .arg(build)
        .args(["--config", "Release"])
        .args(["--target", "install"])
        .args(["-j", &num_cpus::get().to_string()]);
    run_command(&mut build_cmd, "cmake (build)");
}

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").unwrap() != "windows" {
        panic!("alembic-sys only supports Windows");
    }
    let crt_static = env::var("CARGO_CFG_TARGET_FEATURE")
        .map(|features| features.split(',').any(|f| f == "crt-static"))
        .unwrap_or(false);

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // Allow reusing a prebuilt Imath+Alembic install (dev iteration / CI cache).
    println!("cargo:rerun-if-env-changed=ALEMBIC_INSTALL_DIR");
    let install_dir = match env::var("ALEMBIC_INSTALL_DIR") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => {
            let install_dir = out_dir.join("alembic-install");
            let runtime_lib = |name: &str| install_dir.join("lib").join(name);
            // Both libs pinned by submodule commit; skip the slow rebuild if present.
            if !runtime_lib("Alembic.lib").exists() {
                let msvc_rt = if crt_static {
                    "-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded$<$<CONFIG:Debug>:Debug>".to_owned()
                } else {
                    "-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreadedDLL$<$<CONFIG:Debug>:Debug>".to_owned()
                };
                cmake_install(
                    &manifest_dir.join("Imath"),
                    &out_dir.join("imath-build"),
                    &install_dir,
                    &[
                        "-DBUILD_SHARED_LIBS=OFF".to_owned(),
                        "-DBUILD_TESTING=OFF".to_owned(),
                        "-DIMATH_INSTALL_PKG_CONFIG=OFF".to_owned(),
                        msvc_rt.clone(),
                    ],
                );
                cmake_install(
                    &manifest_dir.join("alembic"),
                    &out_dir.join("alembic-build"),
                    &install_dir,
                    &[
                        "-DALEMBIC_SHARED_LIBS=OFF".to_owned(),
                        "-DUSE_HDF5=OFF".to_owned(),
                        "-DUSE_TESTS=OFF".to_owned(),
                        "-DALEMBIC_NO_TESTS=ON".to_owned(),
                        "-DALEMBIC_BUILD_LIBS=ON".to_owned(),
                        msvc_rt,
                    ],
                );
            }
            install_dir
        }
    };

    let include = install_dir.join("include");
    let mut bridge = cc::Build::new();
    bridge
        .cpp(true)
        .file("bridge.cpp")
        .include(&include)
        .flag("/std:c++17")
        .flag("/EHsc")
        .static_crt(crt_static);
    bridge.compile("alembic_bridge");

    println!(
        "cargo:rustc-link-search=native={}",
        install_dir.join("lib").display()
    );
    // Imath's installed lib name embeds the version; discover it rather than
    // hardcoding so a submodule bump doesn't silently break linking.
    let imath_lib = std::fs::read_dir(install_dir.join("lib"))
        .unwrap()
        .filter_map(|e| e.ok())
        .find_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (name.starts_with("Imath") && name.ends_with(".lib"))
                .then(|| name.trim_end_matches(".lib").to_owned())
        })
        .expect("Imath static lib not found in install dir");
    println!("cargo:rustc-link-lib=static=Alembic");
    println!("cargo:rustc-link-lib=static={}", imath_lib);

    println!("cargo:rerun-if-changed=bridge.cpp");
    println!("cargo:rerun-if-changed=src/lib.rs");
}
