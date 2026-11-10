//! Resolves a YouTube video id to a direct audio URL plus the HTTP headers
//! googlevideo requires (without them it answers 403).

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    pub url: String,
    pub headers: Vec<(String, String)>,
}

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
    parse(&out.stdout)
}

fn parse(json: &[u8]) -> Result<Stream, String> {
    #[derive(serde::Deserialize)]
    struct Raw {
        url: Option<String>,
        http_headers: Option<std::collections::BTreeMap<String, String>>,
    }
    let raw: Raw = serde_json::from_slice(json).map_err(|e| format!("bad yt-dlp output: {e}"))?;
    Ok(Stream {
        url: raw.url.ok_or("yt-dlp output has no url")?,
        headers: raw.http_headers.unwrap_or_default().into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixture() {
        let s = parse(include_bytes!("../tests/fixtures/ytdlp.json")).unwrap();
        assert!(s.url.starts_with("https://"));
        assert!(s.headers.iter().any(|(k, _)| k == "User-Agent"));
    }

    #[test]
    fn rejects_missing_url() {
        assert!(parse(b"{}").is_err());
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
