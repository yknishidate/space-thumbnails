use std::{path::PathBuf, time::Instant};

use clap::{ArgEnum, Parser};
use image::{ImageBuffer, Rgba};
use space_thumbnails::{SpaceThumbnailsRenderer, RendererBackend};

/// A command line tool for generating thumbnails for 3D model files.
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// The output file
    output: PathBuf,

    // The 3D model file for which you want to generate thumbnail.
    #[clap(short, long)]
    input: PathBuf,

    // Specify the backend API
    #[clap(short, long, arg_enum, default_value_t)]
    api: BackendApi,

    // Generated thumbnail width
    #[clap(short, long, default_value_t = 800)]
    width: u32,

    // Generated thumbnail height
    #[clap(short, long, default_value_t = 800)]
    height: u32,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ArgEnum)]
enum BackendApi {
    Default,
    OpenGL,
    Vulkan,
    Metal,
}

impl Default for BackendApi {
    fn default() -> Self {
        Self::Default
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();

    let total_start = Instant::now();

    let stage_start = Instant::now();
    let mut renderer = SpaceThumbnailsRenderer::new(
        match args.api {
            BackendApi::Default => RendererBackend::Default,
            BackendApi::OpenGL => RendererBackend::OpenGL,
            BackendApi::Vulkan => RendererBackend::Vulkan,
            BackendApi::Metal => RendererBackend::Metal,
        },
        args.width,
        args.height,
    );
    log::info!("[cli] renderer init: {:.2?}", stage_start.elapsed());

    let stage_start = Instant::now();
    renderer.load_asset_from_file(&args.input).unwrap();
    log::info!("[cli] load asset: {:.2?}", stage_start.elapsed());

    let stage_start = Instant::now();
    let mut screenshot_buffer = vec![0; renderer.get_screenshot_size_in_byte()];
    renderer.take_screenshot_sync(screenshot_buffer.as_mut_slice());
    log::info!("[cli] render + readback: {:.2?}", stage_start.elapsed());

    let stage_start = Instant::now();
    let image = ImageBuffer::<Rgba<u8>, _>::from_raw(args.width, args.height, screenshot_buffer).unwrap();
    image.save(args.output).unwrap();
    log::info!("[cli] encode + save: {:.2?}", stage_start.elapsed());

    log::info!("[cli] total: {:.2?}", total_start.elapsed());
}
