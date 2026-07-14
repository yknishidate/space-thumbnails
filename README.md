# Space Thumbnails

Generates preview thumbnails for 3D model files. Provide a Windows Explorer extensions that adds preview thumbnails for 3D model files.

![screenshot](./screenshot.png)

## Supported formats

- Wavefront Object (`.obj`)
- FBX-Format, as ASCII and binary (`.fbx`)
- Alembic (`.abc`)
- Stereolithography (`.stl`)
- Collada (`.dae`)
- Stanford Polygon Library (`.ply`)
- glTF2.0 (`.glb`/`.glTF`)
- VRM avatars (`.vrm`, VRM 0.x and VRM 1.0)
- Extensible 3D (`.x3d`/`.x3db`)
- 3D Studio Max 3DS (`.3ds`)
- MaterialX (`.mtlx`)

## Windows Explorer Extensions

#### **[Download From Github Release](https://github.com/yknishidate/space-thumbnails/releases)**

[![](https://img.shields.io/github/v/release/yknishidate/space-thumbnails?display_name=tag&sort=semver)](https://github.com/yknishidate/space-thumbnails/releases)

### No thumbnails showing

**Ensure thumbnails are generally enabled.** Are thumbnails working with other file types on your system, e.g. photos? If not, you may have disabled them altogether.

1. open any folder
2. open the `Folder Options`

   - Windows 10: select `View` → `Options` → `Change folder and search options`

   - Windows 7: select `Organize` → `Folder and search options`

3. Select the `View` tab
4. in `Advanced settings`, make sure the `Always show icons, never thumbnails` option is not checked

**Clear your thumbnail cache.** This forces Explorer to request new thumbnails instead of relying on outdated data.

1. click the `Start` button and type `cleanmgr.exe`
2. select drive `C:` and confirm
3. check `Thumbnails` and confirm
4. reboot

### Speed

Rendering thumbnails for 3D models may not be that fast. To keep your explorer smooth and available, we have made some limits here, if the model file size is larger than `300MB` or takes longer than `5 seconds` to load and render, it will be cancelled and display this image below.

<img src="crates/windows/assets/timeout256x256.png" width="100px" />

If there is an error loading the file (corrupt or illegal file), it will display this image below.

<img src="crates/windows/assets/error256x256.png" width="100px" />

### Logs

Space Thumbnails records important warnings and errors in the Windows
Application Event Log. Routine thumbnail successes and input-file errors are
not written there.

To inspect the events, open **Event Viewer**, select **Windows Logs >
Application**, choose **Filter Current Log...**, and select the event source
**Space Thumbnails**. The most recent events can also be viewed from
PowerShell:

```powershell
Get-WinEvent -FilterHashtable @{
  LogName = 'Application'
  ProviderName = 'Space Thumbnails'
} -MaxEvents 50
```

#### Detailed diagnostic logs

Detailed per-thumbnail logs are disabled in release builds by default. To
temporarily enable them, set `SPACE_THUMBNAILS_LOG` to `error`, `warn`,
`info`, `debug`, or `trace`, then start a new Explorer/shell host process so
it inherits the setting. Development builds default to `debug` logging.

Diagnostic logs are written per process under
`%LOCALAPPDATA%\SpaceThumbnails\Logs`. Each file is limited to 5 MB with one
backup generation, and files older than seven days are removed when logging
starts.

For privacy, logs include the file name but omit its directory by default. Set
`SPACE_THUMBNAILS_LOG_PATHS=1` only when full paths are needed for a local
diagnostic session. Review diagnostic logs before sharing them.

![](event-viewer.png)

## Links

- [google / filament](https://github.com/google/filament): 3D rendering engine, and [the rust bindings](https://github.com/EYHN/rust-filament)
- [assimp](https://github.com/assimp/assimp): Asset import library, provides support for 3D file formats.
- Thanks to @Shomnipotence for the icon design.

## License

© 2022 [EYHN](https://github.com/EYHN)

Please see [LICENSE](./LICENSE).

## Fork notes

This fork includes a few additional improvements:

- Improved thumbnail rendering performance for large batches of files.
- Added thumbnail rendering support for MaterialX (`.mtlx`) materials.
  ![](https://github.com/user-attachments/assets/e1265381-0ffe-4ace-b8ed-f402959239f7)
- Added thumbnail rendering support for Alembic (`.abc`) geometry caches.
  ![](https://github.com/user-attachments/assets/8dd6169a-ed35-42c4-af79-e7edaa842c1b)
- Added PBR texture support for FBX, OBJ and other Assimp-based formats (external and embedded): base color, metallic, roughness, normal, emissive and ambient occlusion maps.
- Added thumbnail support for VRM 0.x and VRM 1.0 avatars, preferring the
  author-provided embedded thumbnail and falling back to 3D rendering.
  ![](https://github.com/user-attachments/assets/d02f9665-0bf9-4ede-88eb-b05cfcc3a12a)
- Fixed crashes when rendering glTF/GLB files.
- Fixed rendering issues with some FBX files.
- Fixed blank thumbnails for some DAE files.
