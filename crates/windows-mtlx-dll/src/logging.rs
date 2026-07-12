use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    sync::Mutex,
    time::SystemTime,
};

use log::{LevelFilter, Log, Metadata, Record};

/// File-only logger writing to `%LOCALAPPDATA%\SpaceThumbnails\perf.log`, the
/// same file (and line format) used by the main provider DLL. Unlike the main
/// DLL this module skips the Windows Event Log: it keeps the optional feature
/// free of event-source registration.
struct FileLogger {
    file: Mutex<File>,
}

pub fn init() {
    let Some(file) = open_log_file() else { return };
    if log::set_boxed_logger(Box::new(FileLogger {
        file: Mutex::new(file),
    }))
    .is_ok()
    {
        log::set_max_level(LevelFilter::Trace);
    }
}

fn open_log_file() -> Option<File> {
    let dir = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("SpaceThumbnails");
    fs::create_dir_all(&dir).ok()?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("perf.log"))
        .ok()
}

impl Log for FileLogger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        let line = format!(
            "{} pid={} tid={:?} {:5} [{}] {}\n",
            humantime::format_rfc3339_millis(SystemTime::now()),
            std::process::id(),
            std::thread::current().id(),
            record.level(),
            record.target(),
            record.args()
        );
        if let Ok(mut file) = self.file.lock() {
            let _ = file.write_all(line.as_bytes());
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            let _ = file.flush();
        }
    }
}
