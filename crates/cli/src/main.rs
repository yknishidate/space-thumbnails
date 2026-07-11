use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

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
    // Can be specified multiple times for benchmarking.
    #[clap(short, long)]
    input: Vec<PathBuf>,

    /// Render the input list N times in one process (for benchmarking)
    #[clap(long, default_value_t = 1)]
    repeat: u32,

    /// Reuse a single renderer for all renders instead of creating a new one per render
    #[clap(long)]
    reuse_renderer: bool,

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

    if args.input.is_empty() {
        eprintln!("error: at least one --input is required");
        std::process::exit(2);
    }

    let backend = match args.api {
        BackendApi::Default => RendererBackend::Default,
        BackendApi::OpenGL => RendererBackend::OpenGL,
        BackendApi::Vulkan => RendererBackend::Vulkan,
        BackendApi::Metal => RendererBackend::Metal,
    };

    let total_start = Instant::now();

    let mut shared_renderer = if args.reuse_renderer {
        let stage_start = Instant::now();
        let renderer = SpaceThumbnailsRenderer::new(backend, args.width, args.height);
        log::info!("[cli] shared renderer init: {:.2?}", stage_start.elapsed());
        Some(renderer)
    } else {
        None
    };

    let mut timings = Vec::new();
    let mut last_screenshot = None;

    for round in 0..args.repeat {
        for input in &args.input {
            let iter_start = Instant::now();

            let mut fresh_renderer;
            let renderer = match shared_renderer.as_mut() {
                Some(renderer) => renderer,
                None => {
                    fresh_renderer = SpaceThumbnailsRenderer::new(backend, args.width, args.height);
                    &mut fresh_renderer
                }
            };

            renderer.load_asset_from_file(input).unwrap();
            let mut screenshot_buffer = vec![0; renderer.get_screenshot_size_in_byte()];
            renderer.take_screenshot_sync(screenshot_buffer.as_mut_slice());

            let elapsed = iter_start.elapsed();
            log::info!(
                "[cli] thumbnail {} (round {}): {:.2?}",
                input.display(),
                round + 1,
                elapsed
            );
            timings.push(elapsed);
            last_screenshot = Some(screenshot_buffer);
        }
    }

    if timings.len() > 1 {
        let first = timings[0];
        let rest_avg = timings[1..].iter().sum::<Duration>() / (timings.len() - 1) as u32;
        log::info!(
            "[cli] summary: first thumbnail {:.2?}, rest average {:.2?} ({} thumbnails)",
            first,
            rest_avg,
            timings.len()
        );
    }
    log::info!("[cli] total: {:.2?}", total_start.elapsed());

    let image =
        ImageBuffer::<Rgba<u8>, _>::from_raw(args.width, args.height, last_screenshot.unwrap())
            .unwrap();
    image.save(args.output).unwrap();
}
