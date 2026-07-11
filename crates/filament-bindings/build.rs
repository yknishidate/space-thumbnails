mod build_support;

use std::{
    collections::hash_map::DefaultHasher,
    env, fs,
    hash::{Hash, Hasher},
    io,
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
};

use build_support::{download, run_command, static_lib_filename, Target};
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};

const NATIVE_CACHE_VERSION: u32 = 1;

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn git_source_revision(source_dir: &Path) -> String {
    let output = Command::new("git")
        .current_dir(source_dir)
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .output();
    let status = output
        .ok()
        .filter(|output| output.status.success())
        .map(|output| output.stdout)
        .unwrap_or_default();

    let head = Command::new("git")
        .current_dir(source_dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned());

    // A dirty Filament checkout is deliberately not cached. The status output
    // does not contain file contents, so reusing it could hide a source edit.
    if status.is_empty() {
        head
    } else {
        format!(
            "{head}-dirty-{}",
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        )
    }
}

fn cache_matches(stamp: &Path, key: &str, required: &[PathBuf]) -> bool {
    fs::read_to_string(stamp).is_ok_and(|stored| stored == key)
        && required.iter().all(|path| path.is_file() || path.is_dir())
}

fn build_from_source(target: Target, crt_static: bool) -> BuildManifest {
    let filament_source_dir = env::current_dir().unwrap().join("filament");
    if filament_source_dir.exists() == false {
        // source dir not exist, try to clone it
        let mut git_clone = Command::new("git");
        git_clone
            .arg("clone")
            .arg("https://github.com/google/filament.git")
            .arg(&filament_source_dir)
            .arg("--depth=1");
        build_support::run_command(&mut git_clone, "git");
    }

    println!("cargo:rerun-if-env-changed=FILAMENT_BUILD_OUT_DIR");
    let out_dir = env::current_dir().unwrap().join(
        env::var("FILAMENT_BUILD_OUT_DIR")
            .or(env::var("OUT_DIR"))
            .unwrap(),
    );
    let library_out_dir = out_dir.join("lib");
    let filament_build_dir = out_dir.join("filament");
    let filament_install_dir = filament_build_dir.join("out");
    fs::create_dir_all(&library_out_dir).unwrap();

    let mut filament_link_libs: Vec<String> = vec![
        "filament",
        "backend",
        "bluevk",
        "bluegl",
        "filabridge",
        "filaflat",
        "smol-v",
        "geometry",
        "ibl",
        "utils",
        "filameshio",
        "meshoptimizer",
        "image",
        "gltfio",
        "gltfio_core",
        "filamat",
        "shaders",
        "dracodec",
        "stb",
        "ktxreader",
        "uberarchive",
        "uberzlib",
        "basis_transcoder",
        "zstd",
        "abseil",
        "mikktspace",
        "perfetto",
        "webpdecoder",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();

    let filament_license = filament_install_dir.join("LICENSE");
    let filament_include = filament_install_dir.join("include");
    let native_stamp = out_dir.join("filament-native.stamp");
    let native_key = format!(
        "v{NATIVE_CACHE_VERSION};source={};target={};crt={crt_static};cflags={};cxxflags={};asmflags={};webp=on;vulkan=on",
        git_source_revision(&filament_source_dir),
        target,
        env::var("CFLAGS").unwrap_or_default(),
        env::var("CXXFLAGS").unwrap_or_default(),
        env::var("ASMFLAGS").unwrap_or_default(),
    );
    let mut required_native = filament_link_libs
        .iter()
        .map(|lib| library_out_dir.join(static_lib_filename(lib)))
        .collect::<Vec<_>>();
    required_native.extend([filament_license.clone(), filament_include.clone()]);
    let rebuild_filament = !cache_matches(&native_stamp, &native_key, &required_native);

    if rebuild_filament {
        // Configure and build Filament only when its source or native settings changed.
        fs::create_dir_all(&filament_build_dir).unwrap();
        let mut filament_cmake = Command::new("cmake");
        filament_cmake
            .current_dir(&filament_build_dir)
            .arg(filament_source_dir.to_str().unwrap())
            .arg(format!("-DCMAKE_BUILD_TYPE={}", "Release"))
            .arg(format!("-DFILAMENT_SKIP_SAMPLES={}", "ON"))
            .arg(format!("-DFILAMENT_SKIP_SDL2={}", "ON"))
            .arg(format!("-DUSE_STATIC_LIBCXX={}", "OFF"))
            .arg(format!("-DFILAMENT_SUPPORTS_VULKAN={}", "ON"))
            .arg(format!("-DFILAMENT_SUPPORTS_WEBP_TEXTURES={}", "ON"))
            .arg(format!(
                "-DCMAKE_INSTALL_PREFIX={}",
                filament_install_dir.to_str().unwrap()
            ))
            .arg(format!("-DDIST_DIR={}", &target.to_string()));

        let mut compiler_flags = String::new();
        let c_flags = env::var("CFLAGS").unwrap_or_default();
        let mut cxx_flags = env::var("CXXFLAGS").unwrap_or_default();
        let asm_flags = env::var("ASMFLAGS").unwrap_or_default();

        compiler_flags += " -DSTB_IMAGE_STATIC -DSTB_IMAGE_IMPLEMENTATION";

        if cfg!(not(target_os = "windows")) {
            // if not windows,  use ninja and clang
            if crt_static {
                panic!("Only windows support crt-static")
            }

            filament_cmake.env(
                "CMAKE_GENERATOR",
                env::var("CMAKE_GENERATOR").unwrap_or("Ninja".to_string()),
            );

            if cfg!(target_os = "linux") {
                filament_cmake.env("CC", env::var("CC").unwrap_or("clang".to_string()));
                filament_cmake.env("CXX", env::var("CXX").unwrap_or("clang++".to_string()));
                filament_cmake.env("ASM", env::var("ASM").unwrap_or("clang".to_string()));
                cxx_flags += " -stdlib=libc++";
            }
        } else {
            // if windows
            if target.abi == Some("gnu".to_owned()) {
                panic!("MinGW is not supported");
            }

            filament_cmake.arg(format!(
                "-DUSE_STATIC_CRT={}",
                if crt_static { "ON" } else { "OFF" }
            ));

            match target.architecture.as_str() {
                "x86_64" => filament_cmake.args(["-A", "x64"]),
                "i686" => filament_cmake.args(["-A", "Win32"]),
                _ => panic!("Unsupported architecture"),
            };
        }

        filament_cmake.env("CFLAGS", format!("{} {}", compiler_flags, c_flags));
        filament_cmake.env("CXXFLAGS", format!("{} {}", compiler_flags, cxx_flags));
        filament_cmake.env("ASMFLAGS", format!("{} {}", compiler_flags, asm_flags));

        run_command(&mut filament_cmake, "cmake");

        // build filament
        let mut filament_cmake_install = Command::new("cmake");
        filament_cmake_install
            .current_dir(&filament_build_dir)
            .args(["--build", "."])
            .args(["--target", "install"])
            .args(["--config", "Release"]);
        filament_cmake_install.args([
            "--parallel",
            &env::var("NUM_JOBS").unwrap_or(num_cpus::get().to_string()),
        ]);

        run_command(&mut filament_cmake_install, "cmake");

        let filament_native_lib = filament_install_dir.join("lib").join(&target.to_string());

        for lib in filament_link_libs.iter() {
            let source = if lib == "webpdecoder" {
                let filename = if cfg!(target_os = "windows") {
                    "libwebpdecoder.lib".to_owned()
                } else {
                    static_lib_filename(lib)
                };
                filament_install_dir.join("lib").join(filename)
            } else {
                filament_native_lib.join(static_lib_filename(lib))
            };
            fs::copy(source, library_out_dir.join(static_lib_filename(lib))).unwrap();
        }
        fs::write(&native_stamp, &native_key).unwrap();
    } else {
        println!("Filament native build is up to date; skipping CMake build");
    }

    // Rebuild the small C++ bridge independently from the Filament libraries.
    let bridge_stamp = out_dir.join("filament-bridge.stamp");
    let bridge_key = format!(
        "{native_key};bindings={}",
        hash_bytes(&fs::read("bindings.cpp").unwrap())
    );
    let bindings_library = library_out_dir.join(static_lib_filename("bindings"));
    if !cache_matches(&bridge_stamp, &bridge_key, &[bindings_library]) {
        let mut cc_build = cc::Build::new();
        cc_build.file("bindings.cpp");
        cc_build.include(&filament_include);
        cc_build.cpp(true);
        if crt_static {
            cc_build.static_crt(true);
        }
        cc_build.target(&target.to_string());
        cc_build.out_dir(&library_out_dir);
        cc_build.cargo_metadata(false);
        cc_build.warnings(false);

        if cfg!(target_os = "linux") {
            cc_build.compiler(PathBuf::from("clang++"));
        }
        if cfg!(target_os = "windows") {
            cc_build.flag("/std:c++latest");
        } else {
            cc_build.flag("-std=c++17");
            cc_build.cpp_set_stdlib("c++");
        }

        cc_build.compile("bindings");
        fs::write(&bridge_stamp, &bridge_key).unwrap();
    } else {
        println!("Filament C++ bridge is up to date; skipping compilation");
    }
    filament_link_libs.push("bindings".to_owned());

    println!("cargo:rerun-if-changed=bindings.cpp");

    let bindings_rs = out_dir.join("bindings.rs");
    fs::copy("src/bindings.rs", &bindings_rs).expect("Couldn't copy committed bindings");
    println!("cargo:rerun-if-changed=src/bindings.rs");

    BuildManifest {
        link_search_dir: library_out_dir,
        filament_license,
        link_libs: filament_link_libs,
        bindings_rs,
        target: target.to_string(),
    }
}

fn unpack(package: impl AsRef<Path>) -> BuildManifest {
    let unpack_dir = PathBuf::from(env::var("OUT_DIR").unwrap()).join("unpack");
    fs::create_dir_all(&unpack_dir).unwrap();

    let file = fs::File::open(package).unwrap();
    let mut tar_archive = tar::Archive::new(GzDecoder::new(file));

    tar_archive.unpack(&unpack_dir).unwrap();

    let manifest_json = unpack_dir.join("manifest.json");
    let manifest: BuildManifest =
        serde_json::from_reader(io::BufReader::new(fs::File::open(manifest_json).unwrap()))
            .unwrap();

    BuildManifest {
        link_search_dir: unpack_dir.join(manifest.link_search_dir),
        filament_license: unpack_dir.join(manifest.filament_license),
        link_libs: manifest.link_libs.clone(),
        bindings_rs: unpack_dir.join(manifest.bindings_rs),
        target: manifest.target.clone(),
    }
}

fn install(manifest: &BuildManifest) {
    println!(
        "cargo:rustc-link-search=native={}",
        manifest.link_search_dir.display().to_string()
    );

    for lib in &manifest.link_libs {
        println!("cargo:rustc-link-lib=static={}", lib);
    }

    if cfg!(target_os = "linux") {
        println!("cargo:rustc-link-lib={}", "c++");
    }

    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-lib={}", "c++");
        println!("cargo:rustc-link-lib={}", "framework=Metal");
        println!("cargo:rustc-link-lib={}", "framework=CoreVideo");
        println!("cargo:rustc-link-lib={}", "framework=Cocoa");
    }

    if cfg!(target_os = "windows") {
        println!("cargo:rustc-link-lib={}", "gdi32");
        println!("cargo:rustc-link-lib={}", "user32");
        println!("cargo:rustc-link-lib={}", "opengl32");
        println!("cargo:rustc-link-lib={}", "shlwapi");
        println!("cargo:rustc-link-lib={}", "shell32");
    }

    // Write the bindings to the src/bindings.rs file.
    let bindings_dir = PathBuf::from(env::var("OUT_DIR").unwrap()).join("bindings");
    fs::create_dir_all(&bindings_dir).unwrap();
    let bindings_path = bindings_dir.join("bindings.rs");
    fs::copy(&manifest.bindings_rs, bindings_path).unwrap();
}

fn package(manifest: &BuildManifest, output: impl AsRef<Path>) {
    let file = fs::File::create(output).unwrap();
    let enc = GzEncoder::new(file, Compression::default());
    let mut tar_builder = tar::Builder::new(enc);

    tar_builder
        .append_file(
            "bindings.rs",
            &mut fs::File::open(&manifest.bindings_rs).unwrap(),
        )
        .unwrap();
    tar_builder
        .append_file(
            "LICENSE",
            &mut fs::File::open(&manifest.filament_license).unwrap(),
        )
        .unwrap();

    tar_builder
        .append_dir("lib", &manifest.link_search_dir)
        .unwrap();

    for lib_name in &manifest.link_libs {
        let filename = static_lib_filename(&lib_name);
        tar_builder
            .append_file(
                format!("lib/{}", filename),
                &mut fs::File::open(&manifest.link_search_dir.join(filename)).unwrap(),
            )
            .unwrap();
    }

    let manifest_json = serde_json::to_string(&BuildManifest {
        link_search_dir: PathBuf::from("lib"),
        filament_license: PathBuf::from("LICENSE"),
        link_libs: manifest.link_libs.clone(),
        bindings_rs: PathBuf::from("bindings.rs"),
        target: manifest.target.clone(),
    })
    .unwrap();
    let manifest_json_date = manifest_json.as_bytes();
    let mut header = tar::Header::new_gnu();
    header.set_size(manifest_json_date.len() as u64);
    header.set_cksum();
    header.set_mode(0o644);
    header.set_mtime(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u64,
    );

    tar_builder
        .append_data(&mut header, "manifest.json", manifest_json_date)
        .unwrap();

    tar_builder.finish().unwrap();
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct BuildManifest {
    pub link_search_dir: PathBuf,
    pub filament_license: PathBuf,
    pub link_libs: Vec<String>,
    pub bindings_rs: PathBuf,
    pub target: String,
}

fn cache(cache_tar_name: impl AsRef<str>, version: impl AsRef<str>) -> BuildManifest {
    println!("cargo:rerun-if-env-changed=FILAMENT_BUILD_CACHE_DIR");
    if let Ok(cache_dir) = env::var("FILAMENT_BUILD_CACHE_DIR") {
        println!("cargo:rerun-if-changed={}", cache_dir);
        let package = Path::new(&cache_dir).join(cache_tar_name.as_ref());
        if fs::File::open(&package).is_ok() {
            return unpack(&package);
        }
    }

    let download_url = format!(
        "https://github.com/EYHN/filament-binaries/releases/download/filament-bindings/v{}/{}",
        version.as_ref(),
        cache_tar_name.as_ref()
    );

    println!("Downloading {}", download_url);
    let package = download(cache_tar_name, download_url).expect("Download Failed");
    return unpack(&package);
}

fn main() {
    let linkage = env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or(String::new());
    let crt_static = linkage.contains("crt-static");

    let target = Target::target();
    let version = env::var("CARGO_PKG_VERSION").unwrap();

    let mut feature_suffix = String::new();
    if crt_static {
        feature_suffix.push_str("-crtstatic");
    }
    let cache_tar_name = format!(
        "filament-{}-{}{}.tar.gz",
        version,
        target.to_string(),
        feature_suffix
    );

    println!("cargo:rerun-if-env-changed=FILAMENT_PREBUILT");
    let use_cache = env::var("FILAMENT_PREBUILT").unwrap_or("ON".to_string()) != "OFF"
        && cfg!(feature = "prebuilt");

    let build_manifest = if use_cache {
        cache(&cache_tar_name, &version)
    } else {
        let build_manifest = build_from_source(target, crt_static);

        println!("cargo:rerun-if-env-changed=FILAMENT_BUILD_CACHE_DIR");
        if let Ok(cache_dir) = env::var("FILAMENT_BUILD_CACHE_DIR") {
            fs::create_dir_all(&cache_dir).unwrap();
            let output_tar_path = Path::new(&cache_dir).join(&cache_tar_name);
            package(&build_manifest, output_tar_path);
        }

        build_manifest
    };

    install(&build_manifest)
}
