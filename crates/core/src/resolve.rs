//! Resolves a YouTube video id to a direct audio URL plus the HTTP headers
//! googlevideo requires (without them it answers 403).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

pub use crate::player::state::Stream;

/// Fastest client that worked in measurements (yt-dlp 2026.10.26): `visionos` answers in about
/// 2.5 s instead of 4.5 s, returns audio-only opus, needs no JS challenge and its URLs play at
/// once and seek. `skip=hls,dash` drops two manifest downloads.
// ponytail: client names change with YouTube; any failure falls back to the full extraction.
const FAST_ARGS: [&str; 2] = [
    "--extractor-args",
    "youtube:player_client=visionos;skip=hls,dash",
];

/// Audio only, or video up to 1080p plus the best audio. AV1 last: software decoding it is
/// heavy on older machines.
const AUDIO: &str = "bestaudio/best";
const VIDEO: &str =
    "bestvideo[height<=1080][vcodec!^=av01]+bestaudio/bestvideo[height<=1080]+bestaudio/best";

/// Resolves `video_id`. The fast client first, then the full extraction. `patient` skips the
/// fast path and holds the URL back until its `available_at` time (YouTube answers 403 before
/// it): only for a retry after mpv refused a URL that was handed over at once. `video`: a
/// video stream plus a separate audio one instead of audio only.
pub async fn resolve(
    video_id: &str,
    cookies: Option<&Path>,
    patient: bool,
    video: bool,
) -> Result<Stream, String> {
    if video_id.is_empty()
        || !video_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(format!("invalid video id: {video_id:?}"));
    }
    // yt-dlp rewrites the jar it is given on exit: hand it a private copy.
    let jar = cookies.map(Jar::copy_of).transpose()?;
    let jar = jar.as_ref().map(|j| j.0.as_path());
    if !patient {
        match run(video_id, jar, video, &FAST_ARGS, Duration::from_secs(12)).await {
            Ok((stream, available_at)) => {
                // Normally already past; once in ~50 a URL was refused (403) when used the
                // instant yt-dlp returned. Never more than 2 s.
                hold_until(video_id, available_at, 2.0).await;
                return Ok(stream);
            }
            Err(e) => log::debug!("fast resolve {video_id} failed, full extraction: {e}"),
        }
    }
    let (stream, available_at) = run(video_id, jar, video, &[], Duration::from_secs(30)).await?;
    if patient {
        hold_until(video_id, available_at, 15.0).await;
    }
    Ok(stream)
}

/// Sleeps until the unix time `available_at` (plus a margin), at most `max` seconds.
async fn hold_until(video_id: &str, available_at: f64, max: f64) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    let wait = (available_at - now + 0.3).clamp(0.0, max);
    if wait > 0.0 {
        log::debug!("available_at wait {video_id} {}ms", (wait * 1000.0) as u64);
        tokio::time::sleep(Duration::from_secs_f64(wait)).await;
    }
}

async fn run(
    video_id: &str,
    jar: Option<&Path>,
    video: bool,
    extra: &[&str],
    limit: Duration,
) -> Result<(Stream, f64), String> {
    let kind = if extra.is_empty() { "full" } else { "fast" };
    let mut cmd = Command::new(bundled("yt-dlp").unwrap_or_else(|| "yt-dlp".into()));
    // Logged in, yt-dlp needs a JS runtime; it only looks for deno on PATH by itself.
    if let Some(deno) = bundled("deno") {
        cmd.arg("--js-runtimes")
            .arg(format!("deno:{}", deno.display()));
    }
    // A GUI process would flash a console window for every resolve.
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let format = if video { VIDEO } else { AUDIO };
    cmd.args(["-j", "-f", format, "--no-playlist", "--no-warnings"]);
    cmd.args(extra);
    if let Some(c) = jar {
        cmd.arg("--cookies").arg(c);
    }
    cmd.arg(format!("https://music.youtube.com/watch?v={video_id}"))
        .stdin(Stdio::null())
        .kill_on_drop(true);

    let t = std::time::Instant::now();
    let out = match tokio::time::timeout(limit, cmd.output()).await {
        Err(_) => return Err(friendly("timed out")),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err("yt-dlp not found on PATH".into());
        }
        Ok(Err(e)) => return Err(format!("failed to run yt-dlp: {e}")),
        Ok(Ok(o)) => o,
    };
    log::debug!(
        "yt-dlp run {video_id} {kind} cookies={} {}ms",
        jar.is_some(),
        t.elapsed().as_millis()
    );
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let line = stderr.lines().rev().find(|l| !l.trim().is_empty());
        let line = line.map_or("yt-dlp failed", |l| l.trim());
        log::debug!("yt-dlp {video_id} {kind} failed: {line}");
        return Err(friendly(line));
    }
    let (mut stream, at) = parse(&out.stdout)?;
    stream.video = video;
    Ok((stream, at))
}

/// A tool shipped next to our executable (the Windows package), else `None` for the one on PATH.
fn bundled(name: &str) -> Option<PathBuf> {
    let exe = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let p = std::env::current_exe().ok()?.parent()?.join(exe);
    p.is_file().then_some(p)
}

/// A yt-dlp error line as one short sentence for the user. The raw line goes to the log.
fn friendly(raw: &str) -> String {
    let mut line = raw.trim();
    line = line.strip_prefix("ERROR:").unwrap_or(line).trim();
    // `[youtube] <id>: ` (also `[youtube:tab]`), possibly after more `[..]` tags.
    while let Some(rest) = line.strip_prefix('[').and_then(|r| r.split_once("] ")) {
        line = rest.1;
        if let Some((id, rest)) = line.split_once(": ") {
            if !id.contains(' ') {
                line = rest;
            }
        }
    }
    let low = line.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| low.contains(w));
    // Order matters: "live stream recording is not available" also says "not available".
    let short = if has(&["requested format"]) {
        // Logged in, yt-dlp uses clients whose streams need a JS runtime to unlock.
        "No playable stream, install deno (yt-dlp needs it) or update yt-dlp"
    } else if has(&["live stream", "live event", "premieres in", "will begin in"]) {
        "Live streams cannot be played"
    } else if has(&["private video"]) {
        "This video is private"
    } else if has(&["premium"]) {
        "Only for YouTube Music Premium"
    } else if has(&[
        "confirm your age",
        "age-restricted",
        "inappropriate for some",
        "sign in",
    ]) {
        "Age-restricted or sign-in required, log in and try again"
    } else if has(&["country", "region", "geo"]) {
        "Not available in your region"
    } else if has(&[
        "unavailable",
        "not available",
        "removed",
        "does not exist",
        "terminated",
        "copyright",
    ]) {
        "This video is unavailable"
    } else if has(&[
        "timed out",
        "unable to download",
        "urlopen",
        "name resolution",
        "unreachable",
        "connection",
    ]) {
        "Network problem, check your connection"
    } else {
        return line.to_string();
    };
    short.into()
}

/// A private (0600) copy of the cookie file, removed on drop.
struct Jar(std::path::PathBuf);

impl Jar {
    fn copy_of(src: &Path) -> Result<Self, String> {
        use std::io::Write;
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("cmusic-jar-{}-{n}", std::process::id()));
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut dst = opts
            .open(&path)
            .map_err(|e| format!("cookie copy failed: {e}"))?;
        let jar = Self(path); // from here on, dropped (removed) on every exit
        let data = std::fs::read(src).map_err(|e| format!("cookie copy failed: {e}"))?;
        dst.write_all(&data)
            .map_err(|e| format!("cookie copy failed: {e}"))?;
        Ok(jar)
    }
}

impl Drop for Jar {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Resolved streams by video id, each valid until its URL expires. Bounded: the oldest entry
/// goes first.
#[derive(Default)]
pub struct Cache {
    /// Insertion order, oldest first.
    entries: Vec<(String, Stream, u64)>,
}

const CACHE_MAX: usize = 64;
/// Expire early: a URL about to lapse must not start a track.
const EXPIRY_MARGIN: u64 = 300;
/// URLs carry `expire=` (6 h in practice); without it, assume this.
const FALLBACK_TTL: u64 = 2 * 3600;

/// Unix time after which `url` is no longer usable, from its own `expire=` parameter.
pub fn expiry(url: &str, now: u64) -> u64 {
    let at = url
        .split_once('?')
        .and_then(|(_, q)| q.split('&').find_map(|kv| kv.strip_prefix("expire=")))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(now + FALLBACK_TTL);
    at.saturating_sub(EXPIRY_MARGIN)
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Cache {
    pub fn get(&mut self, id: &str, now: u64) -> Option<Stream> {
        self.entries.retain(|e| e.2 > now);
        self.entries.iter().find(|e| e.0 == id).map(|e| e.1.clone())
    }

    pub fn put(&mut self, id: &str, stream: Stream, now: u64) {
        self.remove(id);
        let until = expiry(&stream.url, now);
        self.entries.push((id.to_string(), stream, until));
        if self.entries.len() > CACHE_MAX {
            self.entries.remove(0);
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.entries.retain(|e| e.0 != id);
    }
}

/// Returns the stream and its `available_at` unix time (0 when absent). A `video+audio`
/// format has no top-level url, only `requested_formats`: video first, then audio.
fn parse(json: &[u8]) -> Result<(Stream, f64), String> {
    #[derive(serde::Deserialize)]
    struct Raw {
        url: Option<String>,
        http_headers: Option<std::collections::BTreeMap<String, String>>,
        available_at: Option<f64>,
        #[serde(default)]
        requested_formats: Vec<Raw>,
    }
    let raw: Raw = serde_json::from_slice(json).map_err(|e| format!("bad yt-dlp output: {e}"))?;
    let (main, audio, at) = match raw.requested_formats.as_slice() {
        [v, a] => {
            let at = v
                .available_at
                .unwrap_or(0.0)
                .max(a.available_at.unwrap_or(0.0));
            (v, a.url.clone(), at)
        }
        _ => (&raw, None, raw.available_at.unwrap_or(0.0)),
    };
    let stream = Stream {
        url: main.url.clone().ok_or("yt-dlp output has no url")?,
        headers: main
            .http_headers
            .clone()
            .unwrap_or_default()
            .into_iter()
            .collect(),
        audio,
        video: false,
    };
    Ok((stream, at))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yt_dlp_errors_become_short_sentences() {
        for (raw, want) in [
            (
                "ERROR: [youtube] notavalidid: This video is unavailable",
                "This video is unavailable",
            ),
            (
                "ERROR: [youtube] jfKfPfyJRdk: This live stream recording is not available.",
                "Live streams cannot be played",
            ),
            ("ERROR: [youtube] x: Private video. Sign in if you've been granted access", "This video is private"),
            ("ERROR: [youtube] x: Sign in to confirm your age", "Age-restricted or sign-in required, log in and try again"),
            ("ERROR: [youtube] x: This video is only available to Music Premium members", "Only for YouTube Music Premium"),
            ("ERROR: [youtube] x: The uploader has not made this video available in your country", "Not available in your region"),
            ("ERROR: [youtube] x: Unable to download webpage: <urlopen error timed out>", "Network problem, check your connection"),
            ("ERROR: [youtube] fJ9rUzIMcZQ: Requested format is not available. Use --list-formats for a list of available formats", "No playable stream, install deno (yt-dlp needs it) or update yt-dlp"),
            // Unknown: only the prefixes go.
            ("ERROR: [youtube] abc: Something new happened", "Something new happened"),
            ("yt-dlp failed", "yt-dlp failed"),
        ] {
            assert_eq!(friendly(raw), want, "{raw}");
        }
    }

    #[test]
    fn parses_fixture() {
        let (s, _) = parse(include_bytes!("../tests/fixtures/ytdlp.json")).unwrap();
        assert!(s.url.starts_with("https://"));
        assert!(s.headers.iter().any(|(k, _)| k == "User-Agent"));
    }

    #[test]
    fn rejects_missing_url() {
        assert!(parse(b"{}").is_err());
        let (_, at) = parse(br#"{"url":"https://x","available_at":12.5}"#).unwrap();
        assert_eq!(at, 12.5);
        assert!(parse(b"nope").is_err());
    }

    #[test]
    fn video_and_audio_come_from_requested_formats() {
        let json = br#"{"requested_formats":[
            {"url":"https://v","http_headers":{"User-Agent":"ua"},"available_at":5},
            {"url":"https://a","available_at":7}]}"#;
        let (s, at) = parse(json).unwrap();
        assert_eq!(
            (s.url.as_str(), s.audio.as_deref(), at),
            ("https://v", Some("https://a"), 7.0)
        );
        assert_eq!(s.headers, [("User-Agent".to_string(), "ua".to_string())]);
        let (s, _) = parse(br#"{"url":"https://x","requested_formats":[]}"#).unwrap();
        assert_eq!((s.url.as_str(), s.audio), ("https://x", None));
    }

    #[tokio::test]
    async fn rejects_bad_id() {
        assert!(resolve("", None, false, false).await.is_err());
        assert!(resolve("a b;rm", None, false, false).await.is_err());
    }

    #[tokio::test]
    #[ignore = "needs network and yt-dlp"]
    async fn resolves_real_video() {
        let s = resolve("dQw4w9WgXcQ", None, false, false).await.unwrap();
        assert!(s.url.starts_with("https://"), "{}", s.url);
        assert!(!s.headers.is_empty());
    }

    fn stream(url: &str) -> Stream {
        Stream {
            url: url.into(),
            ..Stream::default()
        }
    }

    #[test]
    fn expiry_comes_from_the_url() {
        assert_eq!(expiry("https://h/v?a=1&expire=10000&b=2", 5), 10000 - 300);
        assert_eq!(expiry("https://h/v?xexpire=9", 100), 100 + 7200 - 300);
        assert_eq!(expiry("https://h/v?expire=soon", 100), 100 + 7200 - 300);
        assert_eq!(expiry("https://h/v", 100), 100 + 7200 - 300);
        assert_eq!(expiry("https://h/v?expire=10", 0), 0, "never underflows");
    }

    #[test]
    fn cache_expires_and_evicts() {
        let mut c = Cache::default();
        c.put("a", stream("https://h/v?expire=1000"), 0);
        assert!(c.get("a", 699).is_some());
        assert!(c.get("a", 700).is_none(), "expired a margin before expire=");
        for i in 0..CACHE_MAX + 2 {
            c.put(&format!("t{i}"), stream("https://h/v?expire=99999"), 0);
        }
        assert!(
            c.get("t0", 0).is_none() && c.get("t1", 0).is_none(),
            "oldest evicted"
        );
        assert!(c.get("t2", 0).is_some());
        c.remove("t2");
        assert!(c.get("t2", 0).is_none());
        c.put("t3", stream("https://h/v?expire=88888"), 0);
        assert_eq!(c.entries.iter().filter(|e| e.0 == "t3").count(), 1);
    }

    #[test]
    fn jar_copy_is_private_and_removed() {
        let src = std::env::temp_dir().join(format!("cmusic-jar-src-{}", std::process::id()));
        std::fs::write(&src, "x").unwrap();
        let jar = Jar::copy_of(&src).unwrap();
        let path = jar.0.clone();
        assert_eq!(std::fs::read(&path).unwrap(), b"x");
        #[cfg(unix)]
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(
                &std::fs::metadata(&path).unwrap().permissions()
            ) & 0o777,
            0o600
        );
        drop(jar);
        assert!(!path.exists());
        std::fs::remove_file(src).ok();
    }
}
