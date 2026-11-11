//! Opt-in file logger: set `CMUSIC_LOG=<file>` and every `log` line is appended there as
//! `<secs since start> <LEVEL> <target>: <message>`. Debug for cmusic crates, Warn for the rest;
//! `CMUSIC_LOG_LEVEL=trace|debug|info|warn` overrides the cmusic level.

use log::{LevelFilter, Log, Metadata, Record};
use std::io::Write;
use std::sync::Mutex;
use std::time::Instant;

struct FileLog {
    file: Mutex<std::fs::File>,
    start: Instant,
    ours: LevelFilter,
}

fn limit(target: &str, ours: LevelFilter) -> LevelFilter {
    // ponytail: prefix match; also catches the "mpv" target on purpose (mpv sends warn+ only).
    if target.starts_with("cmusic") {
        ours
    } else {
        LevelFilter::Warn
    }
}

/// Drops the query string of every URL: mpv's own messages quote signed stream URLs (token and
/// IP address), which must never reach a log file.
fn scrub(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut rest = msg;
    while let Some(at) = rest.find("://") {
        let end = rest[at..]
            .find(char::is_whitespace)
            .map_or(rest.len(), |e| at + e);
        out.push_str(&rest[..at]);
        let cut = rest[at..end].find('?').map_or(end, |q| at + q);
        out.push_str(&rest[at..cut]);
        if cut < end {
            out.push_str("?...");
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

impl Log for FileLog {
    fn enabled(&self, m: &Metadata) -> bool {
        m.level() <= limit(m.target(), self.ours)
    }

    fn log(&self, r: &Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let t = self.start.elapsed().as_secs_f64();
        if let Ok(mut f) = self.file.lock() {
            // Written as one buffer and flushed, so a crash loses nothing.
            let msg = scrub(&r.args().to_string());
            let _ = writeln!(f, "{t:.3} {} {}: {msg}", r.level(), r.target());
            let _ = f.flush();
        }
    }

    fn flush(&self) {}
}

/// Installs the logger when `CMUSIC_LOG` is set. Safe to call any number of times.
pub fn init() {
    let Ok(path) = std::env::var("CMUSIC_LOG") else {
        return;
    };
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        eprintln!("CMUSIC_LOG: cannot open {path}");
        return;
    };
    let ours = match std::env::var("CMUSIC_LOG_LEVEL").as_deref() {
        Ok("trace") => LevelFilter::Trace,
        Ok("info") => LevelFilter::Info,
        Ok("warn") => LevelFilter::Warn,
        _ => LevelFilter::Debug,
    };
    let logger = Box::new(FileLog {
        file: Mutex::new(file),
        start: Instant::now(),
        ours,
    });
    // Err means one is already installed: keep it.
    if log::set_logger(Box::leak(logger)).is_ok() {
        log::set_max_level(ours.max(LevelFilter::Warn));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_lose_their_query() {
        assert_eq!(
            scrub(
                "Failed to open https://rr1.googlevideo.com/videoplayback?ip=1.2.3.4&sig=x (x) ok"
            ),
            "Failed to open https://rr1.googlevideo.com/videoplayback?... (x) ok"
        );
        assert_eq!(scrub("no url here"), "no url here");
    }

    #[test]
    fn levels_per_target() {
        assert_eq!(
            limit("cmusic_core::engine", LevelFilter::Debug),
            LevelFilter::Debug
        );
        assert_eq!(limit("reqwest::x", LevelFilter::Debug), LevelFilter::Warn);
    }
}
