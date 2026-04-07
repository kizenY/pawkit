//! Simple file logger for production builds.
//! In debug mode, also prints to stdout. In release mode, only writes to file.
//! Log file: %APPDATA%/pawkit/pawkit.log (Windows) or ~/.local/share/pawkit/pawkit.log

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

static LOG_FILE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Max log file size before rotation (2MB)
const MAX_LOG_SIZE: u64 = 2 * 1024 * 1024;

pub fn init() {
    let dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pawkit");
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("pawkit.log");

    // Rotate if too large
    if let Ok(meta) = fs::metadata(&path) {
        if meta.len() > MAX_LOG_SIZE {
            let old = dir.join("pawkit.log.old");
            let _ = fs::rename(&path, &old);
        }
    }

    *LOG_FILE.lock().unwrap() = Some(path);
}

pub fn log_path() -> Option<PathBuf> {
    LOG_FILE.lock().unwrap().clone()
}

pub fn write_log(msg: &str) {
    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let line = format!("[{}] {}\n", timestamp, msg);

    // Always print in debug mode
    #[cfg(debug_assertions)]
    eprint!("{}", line);

    let guard = LOG_FILE.lock().unwrap();
    if let Some(ref path) = *guard {
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

/// Use this macro instead of println! for persistent logging.
#[macro_export]
macro_rules! plog {
    ($($arg:tt)*) => {
        $crate::logger::write_log(&format!($($arg)*))
    };
}
