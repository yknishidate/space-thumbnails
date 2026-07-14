//! Product and diagnostic logging shared by the thumbnail provider DLLs.
//!
//! Warnings and errors go to the Windows Application Event Log. Detailed
//! file logging is opt-in in release builds through `SPACE_THUMBNAILS_LOG`
//! and defaults to `debug` in development builds.

use std::{
    borrow::Cow,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Mutex, Once, OnceLock},
    time::{Duration, SystemTime},
};

use log::{Level, LevelFilter, Log, Metadata, Record};

const EVENT_LOG_LEVEL: Level = Level::Warn;
const MAX_LOG_FILE_BYTES: u64 = 5 * 1024 * 1024;
const LOG_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

struct ProductLogger {
    component: &'static str,
    eventlog: Option<eventlog::EventLog>,
    file: Option<Mutex<RollingFile>>,
    file_level: Option<LevelFilter>,
}

/// Initializes logging outside `DllMain`.
///
/// Event Log initialization and diagnostic file initialization are
/// independent, so one destination remains usable if the other fails.
pub fn init(component: &'static str) {
    static INIT: Once = Once::new();
    INIT.call_once(|| init_once(component));
}

fn init_once(component: &'static str) {
    let eventlog = eventlog::EventLog::new("Space Thumbnails", EVENT_LOG_LEVEL).ok();
    let file_level = diagnostic_level();
    let file = file_level
        .and_then(|_| RollingFile::open(component).ok())
        .map(Mutex::new);

    if eventlog.is_none() && file.is_none() {
        return;
    }

    let max_level = file_level
        .unwrap_or(LevelFilter::Off)
        .max(LevelFilter::Warn);
    if log::set_boxed_logger(Box::new(ProductLogger {
        component,
        eventlog,
        file,
        file_level,
    }))
    .is_ok()
    {
        log::set_max_level(max_level);
    }
}

/// A privacy-preserving input label for logs. Directory names are omitted by
/// default; full paths are included only when
/// `SPACE_THUMBNAILS_LOG_PATHS=1` is explicitly set.
pub fn input_label(path: &str) -> Cow<'_, str> {
    if include_paths() {
        return Cow::Borrowed(path);
    }
    let filename = Path::new(path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("<unknown>");
    Cow::Owned(filename.to_owned())
}

/// Avoids leaking paths embedded in parser error messages unless explicitly
/// requested for a diagnostic session.
pub fn error_label(error: &io::Error) -> String {
    if include_paths() {
        error.to_string()
    } else {
        format!("{:?}", error.kind())
    }
}

fn include_paths() -> bool {
    static INCLUDE_PATHS: OnceLock<bool> = OnceLock::new();
    *INCLUDE_PATHS.get_or_init(|| {
        std::env::var("SPACE_THUMBNAILS_LOG_PATHS")
            .map(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false)
    })
}

fn diagnostic_level() -> Option<LevelFilter> {
    match std::env::var("SPACE_THUMBNAILS_LOG") {
        Ok(value) => match value.to_ascii_lowercase().as_str() {
            "off" | "0" | "false" => None,
            "error" => Some(LevelFilter::Error),
            "warn" | "warning" => Some(LevelFilter::Warn),
            "info" => Some(LevelFilter::Info),
            "debug" => Some(LevelFilter::Debug),
            "trace" => Some(LevelFilter::Trace),
            _ => None,
        },
        Err(_) if cfg!(debug_assertions) => Some(LevelFilter::Debug),
        Err(_) => None,
    }
}

impl Log for ProductLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        (self.eventlog.is_some() && metadata.level() <= EVENT_LOG_LEVEL)
            || self
                .file_level
                .map(|level| metadata.level() <= level)
                .unwrap_or(false)
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        if record.level() <= EVENT_LOG_LEVEL {
            if let Some(eventlog) = &self.eventlog {
                eventlog.log(record);
            }
        }

        if self
            .file_level
            .map(|level| record.level() <= level)
            .unwrap_or(false)
        {
            if let Some(file) = &self.file {
                let line = format!(
                    "{} pid={} tid={:?} {:5} component={} target={} {}\n",
                    humantime::format_rfc3339_millis(SystemTime::now()),
                    std::process::id(),
                    std::thread::current().id(),
                    record.level(),
                    self.component,
                    record.target(),
                    record.args()
                );
                if let Ok(mut file) = file.lock() {
                    let _ = file.write(line.as_bytes());
                }
            }
        }
    }

    fn flush(&self) {
        if let Some(file) = &self.file {
            if let Ok(mut file) = file.lock() {
                let _ = file.flush();
            }
        }
    }
}

struct RollingFile {
    path: PathBuf,
    backup_path: PathBuf,
    file: Option<File>,
    bytes_written: u64,
}

impl RollingFile {
    fn open(component: &str) -> io::Result<Self> {
        let directory = PathBuf::from(
            std::env::var_os("LOCALAPPDATA")
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA is unset"))?,
        )
        .join("SpaceThumbnails")
        .join("Logs");
        fs::create_dir_all(&directory)?;
        remove_expired_logs(&directory);

        let filename = format!("{}-{}.log", component, std::process::id());
        let path = directory.join(filename);
        let backup_path = path.with_extension("log.1");
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let bytes_written = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        Ok(Self {
            path,
            backup_path,
            file: Some(file),
            bytes_written,
        })
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.bytes_written.saturating_add(bytes.len() as u64) > MAX_LOG_FILE_BYTES {
            self.rotate()?;
        }
        let file = self.file.as_mut().expect("rolling log file is open");
        file.write_all(bytes)?;
        self.bytes_written = self.bytes_written.saturating_add(bytes.len() as u64);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.file.as_mut() {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }

    fn rotate(&mut self) -> io::Result<()> {
        if let Some(mut file) = self.file.take() {
            let _ = file.flush();
        }
        let _ = fs::remove_file(&self.backup_path);
        if self.path.exists() {
            fs::rename(&self.path, &self.backup_path)?;
        }
        self.file = Some(
            OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&self.path)?,
        );
        self.bytes_written = 0;
        Ok(())
    }
}

fn remove_expired_logs(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_log = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.ends_with(".log") || name.ends_with(".log.1"))
            .unwrap_or(false);
        if !is_log {
            continue;
        }
        let expired = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .and_then(|modified| {
                SystemTime::now()
                    .duration_since(modified)
                    .map_err(io::Error::other)
            })
            .map(|age| age > LOG_RETENTION)
            .unwrap_or(false);
        if expired {
            let _ = fs::remove_file(path);
        }
    }
}
