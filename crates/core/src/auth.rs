//! One Netscape `cookies.txt` is the single credential store: yt-dlp reads it with `--cookies`
//! and the `Cookie` header for the YouTube Music API is derived from it.

use crate::types::CookieSource;
use std::io::Write;
use std::path::{Path, PathBuf};

const HINT: &str = "use `cmusic login --paste` or `--file`";
const HTTP_ONLY: &str = "#HttpOnly_";

/// One cookie line of a Netscape file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    domain: String,
    subdomains: bool,
    path: String,
    secure: bool,
    expires: u64,
    name: String,
    value: String,
    http_only: bool,
}

impl Row {
    fn is_youtube(&self) -> bool {
        let d = self.domain.trim_start_matches('.');
        d == "youtube.com" || d.ends_with(".youtube.com")
    }
}

pub fn cookie_path() -> PathBuf {
    directories::ProjectDirs::from("", "", "cmusic")
        .map(|d| d.config_dir().join("cookies.txt"))
        .unwrap_or_else(|| PathBuf::from("cookies.txt"))
}

pub fn import(source: &CookieSource) -> Result<(), String> {
    import_to(&cookie_path(), source)
}

/// `Cookie` header value for youtube.com, `None` if there is no file or no SAPISID.
pub fn cookie_header() -> Option<String> {
    header_from(&cookie_path())
}

pub fn logged_in() -> bool {
    cookie_header().is_some()
}

pub fn logout() -> std::io::Result<()> {
    match std::fs::remove_file(cookie_path()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn import_to(path: &Path, source: &CookieSource) -> Result<(), String> {
    let rows = match source {
        CookieSource::Browsers => from_browsers()?,
        CookieSource::Header(s) => parse_header(s),
        CookieSource::File(p) => {
            let text = std::fs::read_to_string(p)
                .map_err(|e| format!("cannot read {}: {e}", p.display()))?;
            let rows = parse_netscape(&text);
            if rows.is_empty() {
                return Err(format!("{} is not a Netscape cookies.txt", p.display()));
            }
            rows
        }
    };
    if !has_sapisid(&rows) {
        return Err(
            "no SAPISID cookie for youtube.com found (it is what the API login is built from); \
             make sure you are signed in to music.youtube.com"
                .into(),
        );
    }
    write_atomic(path, &render(&rows)).map_err(|e| format!("cannot save cookies: {e}"))
}

fn has_sapisid(rows: &[Row]) -> bool {
    rows.iter()
        .any(|r| r.is_youtube() && (r.name == "SAPISID" || r.name == "__Secure-3PAPISID"))
}

fn from_browsers() -> Result<Vec<Row>, String> {
    type Reader = fn(Option<Vec<String>>) -> rookie::Result<Vec<rookie::enums::Cookie>>;
    let browsers: [(&str, Reader); 4] = [
        ("Firefox", rookie::firefox),
        ("Chrome", rookie::chrome),
        ("Edge", rookie::edge),
        ("Brave", rookie::brave),
    ];
    let mut errors = Vec::new();
    for (name, read) in browsers {
        match read(Some(vec!["youtube.com".into()])) {
            Ok(cookies) => {
                let rows: Vec<Row> = cookies
                    .into_iter()
                    .map(|c| Row {
                        subdomains: c.domain.starts_with('.'),
                        domain: c.domain,
                        path: c.path,
                        secure: c.secure,
                        expires: c.expires.unwrap_or(0),
                        name: c.name,
                        value: c.value,
                        http_only: c.http_only,
                    })
                    .collect();
                if has_sapisid(&rows) {
                    return Ok(rows);
                }
                errors.push(format!("{name}: no signed-in YouTube session"));
            }
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }
    Err(format!(
        "no browser had a YouTube login ({}); {HINT}",
        errors.join("; ")
    ))
}

/// Parses a pasted `Cookie:` header value into `.youtube.com` rows.
fn parse_header(s: &str) -> Vec<Row> {
    let quotes = |c: char| c == '"' || c == '\'';
    let mut s = s.trim().trim_matches(quotes).trim();
    if s.len() >= 7 && s[..7].eq_ignore_ascii_case("cookie:") {
        s = &s[7..];
    }
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        + 2 * 365 * 24 * 3600;
    s.split(';')
        .filter_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            let (name, value) = (name.trim(), value.trim());
            // Control characters would corrupt the line-based file format.
            if name.is_empty()
                || name.contains(char::is_control)
                || value.contains(char::is_control)
            {
                return None;
            }
            Some(Row {
                domain: ".youtube.com".into(),
                subdomains: true,
                path: "/".into(),
                secure: true,
                expires,
                name: name.into(),
                value: value.into(),
                http_only: false,
            })
        })
        .collect()
}

fn parse_netscape(text: &str) -> Vec<Row> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim_end_matches('\r');
            // `#HttpOnly_` lines are cookies, every other `#` line is a comment.
            let (line, http_only) = match line.strip_prefix(HTTP_ONLY) {
                Some(rest) => (rest, true),
                None => (line, false),
            };
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let f: Vec<&str> = line.splitn(7, '\t').collect();
            if f.len() < 7 || f[5].is_empty() {
                return None;
            }
            Some(Row {
                domain: f[0].into(),
                subdomains: f[1].eq_ignore_ascii_case("TRUE"),
                path: f[2].into(),
                secure: f[3].eq_ignore_ascii_case("TRUE"),
                expires: f[4].parse().unwrap_or(0),
                name: f[5].into(),
                value: f[6].into(),
                http_only,
            })
        })
        .collect()
}

fn render(rows: &[Row]) -> String {
    let mut out = String::from("# Netscape HTTP Cookie File\n");
    for r in rows {
        let flag = |b: bool| if b { "TRUE" } else { "FALSE" };
        out.push_str(&format!(
            "{}{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            if r.http_only { HTTP_ONLY } else { "" },
            r.domain,
            flag(r.subdomains),
            r.path,
            flag(r.secure),
            r.expires,
            r.name,
            r.value
        ));
    }
    out
}

fn header_from(path: &Path) -> Option<String> {
    let rows: Vec<Row> = parse_netscape(&std::fs::read_to_string(path).ok()?)
        .into_iter()
        .filter(Row::is_youtube)
        .collect();
    let find = |n: &str| rows.iter().find(|r| r.name == n).map(|r| r.value.clone());
    // The API client reads the hash input from `SAPISID=` and needs a `;` after it, so it goes
    // first. __Secure-3PAPISID carries the same value when SAPISID is missing.
    let sapisid = find("SAPISID").or_else(|| find("__Secure-3PAPISID"))?;
    let mut seen = vec!["SAPISID"];
    let mut parts = vec![format!("SAPISID={sapisid}")];
    for r in &rows {
        if !seen.contains(&r.name.as_str()) {
            seen.push(&r.name);
            parts.push(format!("{}={}", r.name, r.value));
        }
    }
    Some(parts.join("; "))
}

/// Temp file in the same dir, created owner-only before any secret goes in, then renamed.
fn write_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = path.with_extension("txt.tmp");
    let _ = std::fs::remove_file(&tmp); // a stale file could carry looser permissions
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let result = (|| {
        let mut f = opts.open(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cmusic-auth-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.join("cookies.txt")
    }

    fn pairs(rows: &[Row]) -> Vec<(&str, &str)> {
        rows.iter()
            .map(|r| (r.name.as_str(), r.value.as_str()))
            .collect()
    }

    #[test]
    fn header_variants() {
        for s in [
            "SAPISID=abc; SID=x=y",
            "Cookie: SAPISID=abc; SID=x=y",
            "  cookie:   SAPISID=abc;SID=x=y ; ",
            "\"Cookie: SAPISID=abc; SID=x=y\"",
            "'SAPISID=abc; SID=x=y'\n",
        ] {
            assert_eq!(
                pairs(&parse_header(s)),
                [("SAPISID", "abc"), ("SID", "x=y")],
                "{s:?}"
            );
        }
        assert!(parse_header("garbage; =novalue").is_empty());
    }

    #[test]
    fn header_import_round_trip() {
        let path = tmp("header");
        let src = CookieSource::Header("Cookie: SID=s; __Secure-3PAPISID=p; PREF=q".into());
        import_to(&path, &src).unwrap();
        // SAPISID is synthesised from __Secure-3PAPISID and placed first.
        assert_eq!(
            header_from(&path).unwrap(),
            "SAPISID=p; SID=s; __Secure-3PAPISID=p; PREF=q"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Netscape HTTP Cookie File\n"));
        assert!(text.contains(".youtube.com\tTRUE\t/\tTRUE\t"));
    }

    #[test]
    fn netscape_round_trip_with_http_only() {
        let text = "# Netscape HTTP Cookie File\n\n\
            #HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t1900000000\tSAPISID\tsecret\n\
            .youtube.com\tTRUE\t/\tFALSE\t0\tPREF\tf=1\n\
            # a comment\n\
            .google.com\tTRUE\t/\tTRUE\t1900000000\tNID\tnope\n";
        let rows = parse_netscape(text);
        assert_eq!(rows.len(), 3);
        assert!(rows[0].http_only && rows[0].name == "SAPISID");
        assert!(!rows[1].http_only && !rows[1].secure);
        assert_eq!(parse_netscape(&render(&rows)), rows);

        let src = tmp("file-src");
        let dst = tmp("file-dst");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, text).unwrap();
        import_to(&dst, &CookieSource::File(src)).unwrap();
        // Only youtube.com cookies make it into the header.
        assert_eq!(header_from(&dst).unwrap(), "SAPISID=secret; PREF=f=1");
        assert!(std::fs::read_to_string(&dst)
            .unwrap()
            .contains("#HttpOnly_.youtube.com"));
    }

    #[test]
    fn rejects_without_sapisid() {
        let path = tmp("nosapisid");
        let err = import_to(&path, &CookieSource::Header("SID=1; PREF=2".into())).unwrap_err();
        assert!(err.contains("SAPISID"), "{err}");
        assert!(!path.exists());

        let src = tmp("nosapisid-src");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        // SAPISID on a non-youtube domain does not count.
        std::fs::write(&src, ".google.com\tTRUE\t/\tTRUE\t0\tSAPISID\tx\n").unwrap();
        assert!(import_to(&path, &CookieSource::File(src.clone())).is_err());
        std::fs::write(&src, "not a cookie file").unwrap();
        let err = import_to(&path, &CookieSource::File(src)).unwrap_err();
        assert!(err.contains("Netscape"), "{err}");
        assert!(!path.exists());
        assert_eq!(header_from(&path), None);
    }

    #[cfg(unix)]
    #[test]
    fn file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let path = tmp("mode");
        import_to(&path, &CookieSource::Header("SAPISID=a; SID=b".into())).unwrap();
        // Overwriting must keep the mode too.
        import_to(&path, &CookieSource::Header("SAPISID=c; SID=d".into())).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
