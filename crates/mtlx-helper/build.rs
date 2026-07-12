use std::{
    env,
    path::PathBuf,
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

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").unwrap() != "windows" {
        panic!("space-thumbnails-mtlx-helper only supports Windows");
    }
    let crt_static = env::var("CARGO_CFG_TARGET_FEATURE")
        .map(|features| features.split(',').any(|f| f == "crt-static"))
        .unwrap_or(false);

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let materialx_source = manifest_dir.join("MaterialX");

    // Allow reusing a prebuilt MaterialX install (dev iteration / CI cache).
    println!("cargo:rerun-if-env-changed=MATERIALX_INSTALL_DIR");
    let install_dir = match env::var("MATERIALX_INSTALL_DIR") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => {
            let install_dir = out_dir.join("materialx-install");
            // MaterialX sources are pinned by the submodule commit, so an
            // existing install is always current; skip the slow cmake build.
            if !install_dir.join("lib").join("MaterialXCore.lib").exists() {
                let build_dir = out_dir.join("materialx-build");
                std::fs::create_dir_all(&build_dir).unwrap();

                let mut configure = Command::new("cmake");
                configure
                    .arg("-S")
                    .arg(&materialx_source)
                    .arg("-B")
                    .arg(&build_dir)
                    .args(["-A", "x64"])
                    .arg("-DCMAKE_BUILD_TYPE=Release")
                    .arg("-DMATERIALX_BUILD_GEN_OSL=OFF")
                    .arg("-DMATERIALX_BUILD_GEN_MDL=OFF")
                    .arg("-DMATERIALX_BUILD_GEN_MSL=OFF")
                    .arg("-DMATERIALX_BUILD_GEN_SLANG=OFF")
                    .arg("-DMATERIALX_BUILD_TESTS=OFF")
                    .arg(format!(
                        "-DCMAKE_INSTALL_PREFIX={}",
                        install_dir.to_str().unwrap()
                    ));
                if crt_static {
                    configure.arg(
                        "-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded$<$<CONFIG:Debug>:Debug>",
                    );
                }
                run_command(&mut configure, "cmake (configure MaterialX)");

                let mut build = Command::new("cmake");
                build
                    .arg("--build")
                    .arg(&build_dir)
                    .args(["--config", "Release"])
                    .args(["--target", "install"])
                    .args(["-j", &num_cpus::get().to_string()]);
                run_command(&mut build, "cmake (build MaterialX)");
            }
            install_dir
        }
    };

    let mut bridge = cc::Build::new();
    bridge
        .cpp(true)
        .file("bridge.cpp")
        .include(install_dir.join("include"))
        .flag("/std:c++17")
        .flag("/EHsc")
        .static_crt(crt_static);
    bridge.compile("mtlx_bridge");

    println!(
        "cargo:rustc-link-search=native={}",
        install_dir.join("lib").display()
    );
    for lib in [
        "MaterialXRenderGlsl",
        "MaterialXRenderHw",
        "MaterialXRender",
        "MaterialXGenGlsl",
        "MaterialXGenHw",
        "MaterialXGenShader",
        "MaterialXFormat",
        "MaterialXCore",
    ] {
        println!("cargo:rustc-link-lib=static={}", lib);
    }
    for lib in ["opengl32", "user32", "gdi32"] {
        println!("cargo:rustc-link-lib={}", lib);
    }

    println!("cargo:rerun-if-changed=bridge.cpp");
}
