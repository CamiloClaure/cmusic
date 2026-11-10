//! Resolves a YouTube video id to a direct audio URL plus the HTTP headers
//! googlevideo requires (without them it answers 403).

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

pub use crate::player::state::Stream;

pub async fn resolve(video_id: &str, cookies: Option<&Path>) -> Result<Stream, String> {
    if video_id.is_empty()
        || !video_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(format!("invalid video id: {video_id:?}"));
    }
    let mut cmd = Command::new("yt-dlp");
    cmd.args([
        "-j",
        "-f",
        "bestaudio/best",
        "--no-playlist",
        "--no-warnings",
    ]);
    if let Some(c) = cookies {
        cmd.arg("--cookies").arg(c);
    }
    cmd.arg(format!("https://music.youtube.com/watch?v={video_id}"))
        .stdin(Stdio::null())
        .kill_on_drop(true);

    let out = match tokio::time::timeout(Duration::from_secs(30), cmd.output()).await {
        Err(_) => return Err("timed out".into()),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err("yt-dlp not found on PATH".into());
        }
        Ok(Err(e)) => return Err(format!("failed to run yt-dlp: {e}")),
        Ok(Ok(o)) => o,
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let line = stderr.lines().rev().find(|l| !l.trim().is_empty());
        let line = line.map_or("yt-dlp failed", |l| l.trim());
        return Err(line.strip_prefix("ERROR: ").unwrap_or(line).to_string());
    }
    let (stream, available_at) = parse(&out.stdout)?;
    // YouTube answers 403 on a stream URL used before its `available_at` time
    // (a few seconds after it is issued), so hold it back until then.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    let wait = (available_at - now + 0.3).clamp(0.0, 15.0);
    tokio::time::sleep(Duration::from_secs_f64(wait)).await;
    Ok(stream)
}

/// Returns the stream and its `available_at` unix time (0 when absent).
fn parse(json: &[u8]) -> Result<(Stream, f64), String> {
    #[derive(serde::Deserialize)]
    struct Raw {
        url: Option<String>,
        http_headers: Option<std::collections::BTreeMap<String, String>>,
        available_at: Option<f64>,
    }
    let raw: Raw = serde_json::from_slice(json).map_err(|e| format!("bad yt-dlp output: {e}"))?;
    let stream = Stream {
        url: raw.url.ok_or("yt-dlp output has no url")?,
        headers: raw.http_headers.unwrap_or_default().into_iter().collect(),
    };
    Ok((stream, raw.available_at.unwrap_or(0.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn rejects_bad_id() {
        assert!(resolve("", None).await.is_err());
        assert!(resolve("a b;rm", None).await.is_err());
    }

    #[tokio::test]
    #[ignore = "needs network and yt-dlp"]
    async fn resolves_real_video() {
        let s = resolve("dQw4w9WgXcQ", None).await.unwrap();
        assert!(s.url.starts_with("https://"), "{}", s.url);
        assert!(!s.headers.is_empty());
    }
}
