mod build_support;

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use build_support::{download, run_command, unzip};
use space_thumbnails_windows::{
    constant::{MTLX_PROVIDER, PROVIDERS},
    providers::Provider,
};

/// Recursively copies `src` into `dst`, skipping directories for which
/// `skip_dir` returns true.
fn copy_tree(src: &Path, dst: &Path, skip_dir: &dyn Fn(&str) -> bool) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let path = entry.path();
        if path.is_dir() {
            if skip_dir(name.to_str().unwrap()) {
                continue;
            }
            copy_tree(&path, &dst.join(&name), skip_dir);
        } else {
            fs::copy(&path, dst.join(&name)).unwrap();
        }
    }
}

/// Stages the MaterialX runtime data (node definition libraries, preview
/// geometry, environment lights) that the helper executable needs, trimmed
/// to what the GLSL pipeline actually uses.
fn stage_mtlx_data(materialx_dir: &Path, staging_dir: &Path) {
    if staging_dir.exists() {
        fs::remove_dir_all(staging_dir).unwrap();
    }

    // Node definitions: everything except the non-GLSL target implementations.
    copy_tree(
        &materialx_dir.join("libraries"),
        &staging_dir.join("libraries"),
        &|name| matches!(name, "genosl" | "genmdl" | "genmsl" | "genslang"),
    );

    for relative in [
        "resources/Geometry/shaderball.glb",
        "resources/Geometry/sphere.obj",
        "resources/Lights/environment_map.mtlx",
        "resources/Lights/san_giuseppe_bridge.hdr",
        "resources/Lights/san_giuseppe_bridge_split.mtlx",
        "resources/Lights/san_giuseppe_bridge_split.hdr",
        "resources/Lights/irradiance/san_giuseppe_bridge.hdr",
        "resources/Lights/irradiance/san_giuseppe_bridge_split.hdr",
        "LICENSE",
    ] {
        let target = staging_dir.join(relative);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(materialx_dir.join(relative), target).unwrap();
    }
}

/// Locates the x64 VC++ redistributable CRT directory
/// (`...\VC\Redist\MSVC\<version>\x64\Microsoft.VCxxx.CRT`) of the newest
/// installed toolset. Honors `VCToolsRedistDir` (set in a VS developer
/// prompt) first, then scans the standard Visual Studio install roots.
fn find_vc_redist_crt_dir() -> PathBuf {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = env::var("VCToolsRedistDir") {
        roots.push(PathBuf::from(dir));
    }
    for program_files in ["C:\\Program Files", "C:\\Program Files (x86)"] {
        let vs_root = Path::new(program_files).join("Microsoft Visual Studio");
        let Ok(years) = fs::read_dir(&vs_root) else {
            continue;
        };
        for year in years.flatten() {
            let Ok(editions) = fs::read_dir(year.path()) else {
                continue;
            };
            for edition in editions.flatten() {
                let msvc = edition.path().join("VC").join("Redist").join("MSVC");
                let Ok(versions) = fs::read_dir(&msvc) else {
                    continue;
                };
                let mut versions: Vec<_> = versions.flatten().map(|e| e.path()).collect();
                versions.sort();
                // newest toolset first
                roots.extend(versions.into_iter().rev());
            }
        }
    }

    for root in roots {
        let Ok(entries) = fs::read_dir(root.join("x64")) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("Microsoft.VC") && name.ends_with(".CRT") {
                return entry.path();
            }
        }
    }
    panic!(
        "VC++ redistributable CRT directory not found; \
         install the Visual Studio C++ workload or set VCToolsRedistDir"
    );
}

/// Emits nested WiX `<Directory>`/`<Component>`/`<File>` elements for every
/// file under `dir`, appending the generated component ids to `component_ids`
/// (for `<ComponentRef>`s).
fn emit_directory_tree(
    wix: &mut String,
    dir: &Path,
    indent: usize,
    counter: &mut u32,
    component_ids: &mut Vec<String>,
) {
    let pad = " ".repeat(indent);
    let mut entries: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_str().unwrap().to_owned();
        if path.is_dir() {
            *counter += 1;
            wix.push_str(&format!(
                "{}<Directory Id=\"MtlxDir{}\" Name=\"{}\">\n",
                pad, counter, name
            ));
            emit_directory_tree(wix, &path, indent + 2, counter, component_ids);
            wix.push_str(&format!("{}</Directory>\n", pad));
        } else {
            *counter += 1;
            let component_id = format!("MtlxData{}", counter);
            wix.push_str(&format!(
                "{}<Component Id=\"{}\" Guid=\"*\" Win64=\"yes\">\n",
                pad, component_id
            ));
            wix.push_str(&format!(
                "{}  <File Id=\"MtlxFile{}\" Source=\"{}\" KeyPath=\"yes\"/>\n",
                pad,
                counter,
                path.to_str().unwrap()
            ));
            wix.push_str(&format!("{}</Component>\n", pad));
            component_ids.push(component_id);
        }
    }
}

fn main() {
    let project_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();

    // Build every binary the MSI packages, so it can never pick up stale
    // artifacts from target\release (CARGO points back at the cargo that
    // launched us).
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let mut build_command = Command::new(cargo);
    build_command.current_dir(&project_dir).args([
        "build",
        "--release",
        "-p",
        "space-thumbnails-windows-dll",
        "-p",
        "space-thumbnails-render-helper",
        "-p",
        "space-thumbnails-mtlx-dll",
        "-p",
        "space-thumbnails-mtlx-helper",
    ]);
    run_command(&mut build_command, "cargo");

    let assets_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");
    let out_dir = project_dir.join("target").join("installer");
    let download_dir = out_dir.join("download");
    fs::create_dir_all(download_dir).unwrap();

    let build_dir = out_dir.join("build");
    fs::create_dir_all(&build_dir).unwrap();

    let registy_keys = PROVIDERS.iter().flat_map(|m| m.register("[#MainDLLFile]"));

    let version = env!("CARGO_PKG_VERSION");

    let mut wix = String::new();
    wix.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    wix.push_str("<Wix xmlns=\"http://schemas.microsoft.com/wix/2006/wi\" xmlns:util=\"http://schemas.microsoft.com/wix/UtilExtension\">\n");
    wix.push_str(&format!("  <Product Id=\"*\" UpgradeCode=\"1C589985-B4C6-53EC-8483-112D02E6DCD2\" Version=\"{}\" Language=\"1033\" Name=\"Space Thumbnails\" Manufacturer=\"EYHN\">\n", version));
    wix.push_str(
        "    <Package InstallerVersion=\"300\" Compressed=\"yes\" InstallScope=\"perMachine\"/>\n",
    );
    wix.push_str("    <Media Id=\"1\" Cabinet=\"cab1.cab\" EmbedCab=\"yes\" />\n");
    wix.push_str("    <Directory Id=\"TARGETDIR\" Name=\"SourceDir\">\n");
    wix.push_str("      <Directory Id=\"ProgramFiles64Folder\">\n");
    wix.push_str(
        "        <Directory Id=\"APPLICATIONROOTDIRECTORY\" Name=\"Space Thumbnails\"/>\n",
    );
    wix.push_str("      </Directory>\n");
    wix.push_str("    </Directory>\n");

    wix.push_str("    <DirectoryRef Id=\"APPLICATIONROOTDIRECTORY\">\n");
    wix.push_str(
        "      <Component Id=\"MainApplication\" Guid=\"9cfa17d1-9a2a-40aa-ba6f-57a2adbdc8dc\" Win64=\"yes\">\n",
    );
    wix.push_str(&format!(
        "        <File Id=\"MainDLLFile\" Source=\"{}\" KeyPath=\"yes\" Checksum=\"yes\"/>\n",
        project_dir
            .join("target\\release\\space_thumbnails_windows_dll.dll")
            .to_str()
            .unwrap()
    ));
    wix.push_str(&format!(
        "        <File Id=\"LicenceFile\" Source=\"{}\" Checksum=\"yes\"/>\n",
        assets_dir.join("Licence.rtf").to_str().unwrap()
    ));
    wix.push_str(&format!(
        "        <File Id=\"ReadmeFile\" Source=\"{}\" Checksum=\"yes\"/>\n",
        project_dir.join("README.md").to_str().unwrap()
    ));
    wix.push_str("        <util:EventSource EventMessageFile=\"[#MainDLLFile]\" Log=\"Application\" Name=\"Space Thumbnails\"/>\n");

    for key in registy_keys {
        wix.push_str(&format!(
            "        <RegistryKey Root=\"HKCR\" Key=\"{}\">\n",
            &key.path
        ));
        for val in key.values {
            let (val_type, val_data) = match val.1 {
                space_thumbnails_windows::registry::RegistryData::Str(data) => ("string", data),
                space_thumbnails_windows::registry::RegistryData::U32(data) => {
                    ("integer", data.to_string())
                }
            };

            if val.0.is_empty() {
                wix.push_str(&format!(
                    "            <RegistryValue Type=\"{}\" Value=\"{}\"/>\n",
                    val_type, val_data
                ));
            } else {
                wix.push_str(&format!(
                    "            <RegistryValue Type=\"{}\" Name=\"{}\" Value=\"{}\"/>\n",
                    val_type, val.0, val_data
                ));
            }
        }
        wix.push_str("        </RegistryKey>\n");
    }

    wix.push_str("      </Component>\n");

    // The out-of-process render helper: every model format (obj/fbx/.../gltf/
    // glb/abc) is rendered here, isolated from explorer.exe. Required by the
    // main feature.
    wix.push_str(
        "      <Component Id=\"RenderHelper\" Guid=\"85233035-9e13-443a-8e89-547075ff4a65\" Win64=\"yes\">\n",
    );
    wix.push_str(&format!(
        "        <File Id=\"RenderHelperFile\" Source=\"{}\" KeyPath=\"yes\" Checksum=\"yes\"/>\n",
        project_dir
            .join("target\\release\\space-thumbnails-render-helper.exe")
            .to_str()
            .unwrap()
    ));
    wix.push_str("      </Component>\n");

    // App-local VC++ runtime: the provider DLLs and helper exes link the CRT
    // dynamically, and a clean Windows install has no VC redistributable.
    // COM loads InProcServer32 DLLs with LOAD_WITH_ALTERED_SEARCH_PATH and
    // the helpers resolve imports from their own directory, so CRT DLLs
    // placed next to the binaries are found first.
    let crt_dir = find_vc_redist_crt_dir();
    let mut crt_files: Vec<PathBuf> = fs::read_dir(&crt_dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .map_or(false, |ext| ext.eq_ignore_ascii_case("dll"))
        })
        .collect();
    crt_files.sort();
    assert!(
        !crt_files.is_empty(),
        "no CRT DLLs found in {}",
        crt_dir.display()
    );
    let mut crt_component_ids = Vec::new();
    for (index, file) in crt_files.iter().enumerate() {
        let component_id = format!("VcCrt{}", index);
        wix.push_str(&format!(
            "      <Component Id=\"{}\" Guid=\"*\" Win64=\"yes\">\n",
            component_id
        ));
        wix.push_str(&format!(
            "        <File Id=\"VcCrtFile{}\" Source=\"{}\" KeyPath=\"yes\" Checksum=\"yes\"/>\n",
            index,
            file.to_str().unwrap()
        ));
        wix.push_str("      </Component>\n");
        crt_component_ids.push(component_id);
    }

    // --- Optional MaterialX (.mtlx) support -------------------------------
    // Separate provider DLL + statically linked helper renderer + the
    // MaterialX runtime data it needs, all grouped under one deselectable
    // feature. Registry keys live here too, so deselecting the feature also
    // leaves the .mtlx shell association untouched.
    let mtlx_staging = out_dir.join("mtlx-data");
    stage_mtlx_data(
        &project_dir
            .join("crates")
            .join("mtlx-helper")
            .join("MaterialX"),
        &mtlx_staging,
    );

    wix.push_str(
        "      <Component Id=\"MtlxApplication\" Guid=\"6ab5b38a-ff92-4737-9f3f-c017eb8923c7\" Win64=\"yes\">\n",
    );
    wix.push_str(&format!(
        "        <File Id=\"MtlxDLLFile\" Source=\"{}\" KeyPath=\"yes\" Checksum=\"yes\"/>\n",
        project_dir
            .join("target\\release\\space_thumbnails_mtlx_dll.dll")
            .to_str()
            .unwrap()
    ));
    for key in MTLX_PROVIDER.register("[#MtlxDLLFile]") {
        wix.push_str(&format!(
            "        <RegistryKey Root=\"HKCR\" Key=\"{}\">\n",
            &key.path
        ));
        for val in key.values {
            let (val_type, val_data) = match val.1 {
                space_thumbnails_windows::registry::RegistryData::Str(data) => ("string", data),
                space_thumbnails_windows::registry::RegistryData::U32(data) => {
                    ("integer", data.to_string())
                }
            };

            if val.0.is_empty() {
                wix.push_str(&format!(
                    "            <RegistryValue Type=\"{}\" Value=\"{}\"/>\n",
                    val_type, val_data
                ));
            } else {
                wix.push_str(&format!(
                    "            <RegistryValue Type=\"{}\" Name=\"{}\" Value=\"{}\"/>\n",
                    val_type, val.0, val_data
                ));
            }
        }
        wix.push_str("        </RegistryKey>\n");
    }
    wix.push_str("      </Component>\n");

    wix.push_str(
        "      <Component Id=\"MtlxHelper\" Guid=\"58228791-cda2-4e54-87a3-b8011da6f7d4\" Win64=\"yes\">\n",
    );
    wix.push_str(&format!(
        "        <File Id=\"MtlxHelperFile\" Source=\"{}\" KeyPath=\"yes\" Checksum=\"yes\"/>\n",
        project_dir
            .join("target\\release\\space-thumbnails-mtlx-helper.exe")
            .to_str()
            .unwrap()
    ));
    wix.push_str("      </Component>\n");

    let mut mtlx_component_ids = vec!["MtlxApplication".to_owned(), "MtlxHelper".to_owned()];
    let mut mtlx_counter = 0u32;
    wix.push_str("      <Directory Id=\"MtlxDataRoot\" Name=\"MaterialX\">\n");
    emit_directory_tree(
        &mut wix,
        &mtlx_staging,
        8,
        &mut mtlx_counter,
        &mut mtlx_component_ids,
    );
    wix.push_str("      </Directory>\n");

    wix.push_str("    </DirectoryRef>\n");

    wix.push_str("    <Feature Id=\"MainFeature\" Title=\"Space Thumbnails\" Description=\"Thumbnails for 3D model files (obj, fbx, stl, dae, ply, x3d, 3ds, gltf, glb, abc).\" Level=\"1\" Absent=\"disallow\" AllowAdvertise=\"no\">\n");
    wix.push_str("      <ComponentRef Id=\"MainApplication\" />\n");
    wix.push_str("      <ComponentRef Id=\"RenderHelper\" />\n");
    for component_id in &crt_component_ids {
        wix.push_str(&format!("      <ComponentRef Id=\"{}\" />\n", component_id));
    }
    wix.push_str("    </Feature>\n");
    wix.push_str("    <Feature Id=\"MaterialXFeature\" Title=\"MaterialX (.mtlx) thumbnails\" Description=\"Renders MaterialX material documents on a preview shader ball. Adds about 19 MB.\" Level=\"1\" Absent=\"allow\" AllowAdvertise=\"no\">\n");
    for component_id in &mtlx_component_ids {
        wix.push_str(&format!("      <ComponentRef Id=\"{}\" />\n", component_id));
    }
    wix.push_str("    </Feature>\n");
    wix.push_str("    <UIRef Id=\"WixUI_FeatureTree\" />\n");
    wix.push_str("    <UIRef Id=\"WixUI_ErrorProgressText\" />\n");
    wix.push_str(&format!(
        "    <Icon Id=\"icon.ico\" SourceFile=\"{}\"/>\n",
        assets_dir.join("icon.ico").to_str().unwrap()
    ));
    wix.push_str("    <Property Id=\"ARPPRODUCTICON\" Value=\"icon.ico\" />\n");
    wix.push_str(&format!(
        "    <WixVariable Id=\"WixUIDialogBmp\" Value=\"{}\" />\n",
        assets_dir.join("UIDialog.bmp").to_str().unwrap()
    ));
    wix.push_str(&format!(
        "    <WixVariable Id=\"WixUIBannerBmp\" Value=\"{}\" />\n",
        assets_dir.join("UIBanner.bmp").to_str().unwrap()
    ));
    wix.push_str(&format!(
        "    <WixVariable Id=\"WixUILicenseRtf\" Value=\"{}\" />\n",
        assets_dir.join("Licence.rtf").to_str().unwrap()
    ));
    // AllowSameVersionUpgrades: every build gets a fresh ProductCode
    // (Product Id="*"), so without this a same-version rebuild installs
    // side by side instead of replacing the previous one.
    wix.push_str("    <MajorUpgrade AllowDowngrades=\"no\" AllowSameVersionUpgrades=\"yes\" DowngradeErrorMessage=\"A newer version of [ProductName] is already installed.  If you are sure you want to downgrade, remove the existing installation via the Control Panel\" />\n");
    wix.push_str("  </Product>\n");
    wix.push_str("</Wix>\n");

    let installerwxs = build_dir.join("installer.wxs");

    fs::write(&installerwxs, wix).unwrap();

    let wixzip = download(
        out_dir.join("download").join("wix311-binaries.zip"),
        "https://github.com/wixtoolset/wix3/releases/download/wix3112rtm/wix311-binaries.zip",
    )
    .unwrap();

    let wixdir = unzip(&wixzip, out_dir.join("wix")).unwrap();

    let mut candle_command = Command::new(wixdir.join("candle.exe"));
    candle_command
        .current_dir(&build_dir)
        .arg(installerwxs.to_str().unwrap())
        .args(["-arch", "x64"])
        .args(["-ext", "WixUtilExtension"]);

    run_command(&mut candle_command, "candle.exe");

    let mut light_command = Command::new(wixdir.join("light.exe"));
    light_command
        .current_dir(&build_dir)
        .arg(build_dir.join("installer.wixobj"))
        .args(["-ext", "WixUIExtension"])
        .args(["-ext", "WixUtilExtension"])
        // LGHT1076/ICE61: same-version upgrade detection is intentional
        .arg("-sw1076");

    run_command(&mut light_command, "light.exe");

    fs::copy(
        build_dir.join("installer.msi"),
        out_dir.join("space-thumbnails-installer.msi"),
    )
    .unwrap();
}
