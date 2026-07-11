use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::Mutex,
    time::SystemTime,
};

use log::{LevelFilter, Log, Metadata, Record};

/// Logger that writes to the Windows Event Log and, if available, to
/// `%LOCALAPPDATA%\SpaceThumbnails\perf.log`. The file log includes pid/tid so
/// the lifetime of the hosting process (explorer.exe / dllhost.exe) can be
/// observed when measuring performance.
struct TeeLogger {
    eventlog: eventlog::EventLog,
    file: Option<Mutex<File>>,
}

pub fn init(name: &str, level: log::Level) {
    let eventlog = match eventlog::EventLog::new(name, level) {
        Ok(logger) => logger,
        Err(_) => return,
    };
    let file = open_log_file().map(Mutex::new);
    if log::set_boxed_logger(Box::new(TeeLogger { eventlog, file })).is_ok() {
        log::set_max_level(LevelFilter::Trace);
    }
}

pub fn log_file_path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("SpaceThumbnails"))
}

fn open_log_file() -> Option<File> {
    let dir = log_file_path()?;
    fs::create_dir_all(&dir).ok()?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("perf.log"))
        .ok()
}

impl Log for TeeLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.eventlog.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        self.eventlog.log(record);

        if let Some(file) = &self.file {
            let line = format!(
                "{} pid={} tid={:?} {:5} [{}] {}\n",
                humantime::format_rfc3339_millis(SystemTime::now()),
                std::process::id(),
                std::thread::current().id(),
                record.level(),
                record.target(),
                record.args()
            );
            if let Ok(mut file) = file.lock() {
                let _ = file.write_all(line.as_bytes());
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
