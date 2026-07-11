use std::{
    collections::hash_map::Entry,
    collections::HashMap,
    io,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, RecvTimeoutError},
        Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use log::{info, warn};
use space_thumbnails::{RendererBackend, SpaceThumbnailsRenderer};

pub enum RenderSource {
    Memory { data: Vec<u8>, filename: String },
    File(String),
}

pub struct RenderRequest {
    pub backend: RendererBackend,
    pub size: u32,
    pub source: RenderSource,
}

type RenderResult = Option<Vec<u8>>;
type Job = (RenderRequest, mpsc::Sender<RenderResult>);

struct Worker {
    generation: u64,
    sender: mpsc::Sender<Job>,
}

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

lazy_static! {
    static ref WORKER: Mutex<Option<Worker>> = Mutex::new(None);
}

/// Render a thumbnail on a long-lived worker thread that owns and reuses
/// renderers (one per backend/size). The worker thread is the only thread
/// touching the filament engine, which keeps its thread affinity intact.
///
/// On timeout the current worker is discarded (the stuck thread keeps running
/// detached, like the old `run_timeout` behavior) and the next request spawns
/// a fresh worker with a fresh renderer.
pub fn render_with_timeout(request: RenderRequest, timeout: Duration) -> io::Result<RenderResult> {
    let mut request = Some(request);

    // The first attempt can fail if a previous worker thread has panicked;
    // retry once with a freshly spawned worker.
    for _ in 0..2 {
        let (generation, sender) = {
            let mut slot = WORKER.lock().unwrap();
            match slot.as_ref() {
                Some(worker) => (worker.generation, worker.sender.clone()),
                None => {
                    let worker = spawn_worker();
                    let cloned = (worker.generation, worker.sender.clone());
                    *slot = Some(worker);
                    cloned
                }
            }
        };

        let (result_sender, result_receiver) = mpsc::channel();
        if let Err(mpsc::SendError(job)) = sender.send((request.take().unwrap(), result_sender)) {
            // worker thread is dead; recover the job and retry with a fresh worker
            discard_worker(generation);
            request = Some(job.0);
            continue;
        }

        return match result_receiver.recv_timeout(timeout) {
            Ok(result) => Ok(result),
            Err(RecvTimeoutError::Timeout) => {
                warn!(target: "RenderWorker", "render timed out, discarding worker generation {}", generation);
                discard_worker(generation);
                Err(io::Error::new(io::ErrorKind::TimedOut, "Timeout"))
            }
            Err(RecvTimeoutError::Disconnected) => {
                warn!(target: "RenderWorker", "render worker panicked (generation {})", generation);
                discard_worker(generation);
                Err(io::Error::new(io::ErrorKind::Other, "Thread panic"))
            }
        };
    }

    Err(io::Error::new(io::ErrorKind::Other, "Render worker dead"))
}

fn discard_worker(generation: u64) {
    let mut slot = WORKER.lock().unwrap();
    if matches!(slot.as_ref(), Some(worker) if worker.generation == generation) {
        *slot = None;
    }
}

fn spawn_worker() -> Worker {
    let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    let (sender, receiver) = mpsc::channel::<Job>();

    thread::spawn(move || {
        info!(target: "RenderWorker", "render worker started (generation {})", generation);
        let mut renderers: HashMap<(u8, u32), SpaceThumbnailsRenderer> = HashMap::new();

        while let Ok((request, result_sender)) = receiver.recv() {
            let key = (request.backend as u8, request.size);
            let renderer = match renderers.entry(key) {
                Entry::Occupied(entry) => {
                    info!(target: "RenderWorker", "renderer cache hit ({:?}, {}px)", request.backend, request.size);
                    entry.into_mut()
                }
                Entry::Vacant(entry) => {
                    let start = Instant::now();
                    let renderer =
                        SpaceThumbnailsRenderer::new(request.backend, request.size, request.size);
                    info!(
                        target: "RenderWorker",
                        "renderer cache miss ({:?}, {}px), created in {:.2?}",
                        request.backend,
                        request.size,
                        start.elapsed()
                    );
                    entry.insert(renderer)
                }
            };

            let result = render(renderer, request.source);
            // free the loaded asset right away so big scenes don't linger in
            // the host process between thumbnail requests
            renderer.destroy_opened_asset();

            if result_sender.send(result).is_err() {
                warn!(target: "RenderWorker", "render result discarded (caller timed out)");
            }
        }

        info!(target: "RenderWorker", "render worker exiting (generation {})", generation);
    });

    Worker { generation, sender }
}

fn render(renderer: &mut SpaceThumbnailsRenderer, source: RenderSource) -> RenderResult {
    match source {
        RenderSource::Memory { data, filename } => {
            renderer.load_asset_from_memory(&data, filename)?;
        }
        RenderSource::File(path) => {
            renderer.load_asset_from_file(path)?;
        }
    }
    let mut buffer = vec![0; renderer.get_screenshot_size_in_byte()];
    renderer.take_screenshot_sync(buffer.as_mut_slice());
    Some(buffer)
}
