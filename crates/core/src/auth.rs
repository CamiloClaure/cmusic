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

/// The stored session is dead (YouTube says so) or lacks cookies.
pub const EXPIRED: &str = "Your YouTube login has expired or is incomplete. Log in again.";

/// What a successful `import_verified` found out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Login {
    /// Saved, and YouTube says it is signed in.
    Verified,
    /// Saved, but YouTube could not be reached to check.
    Unverified,
}

/// Imports a login from `source`. The new set is checked with YouTube first and replaces the
/// stored file only when that does not make things worse: a rejected set is never kept, and
/// when the check cannot run, an existing login is not overwritten. `Err` is a message for
/// the user.
pub async fn import_verified(source: CookieSource) -> Result<Login, String> {
    let rows = tokio::task::spawn_blocking(move || rows_for(&source))
        .await
        .map_err(|e| format!("cookie import failed: {e}"))??;
    let verdict = match header_of(&rows) {
        Some(h) => fetch_logged_in(&h).await,
        None => Some(false),
    };
    save_checked(&cookie_path(), &rows, verdict)
}

/// `import_verified` for code without a runtime (the `login` subcommand).
pub fn import_verified_blocking(source: CookieSource) -> Result<Login, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?
        .block_on(import_verified(source))
}

/// Writes `rows` unless `verdict` (YouTube's answer for them) says not to. See `import_verified`.
fn save_checked(path: &Path, rows: &[Row], verdict: Option<bool>) -> Result<Login, String> {
    let existed = path.exists();
    // A stored login YouTube has not declared dead counts as working.
    let working = header_from(path).is_some() && *CHECK.lock().unwrap() != Some(false);
    let kept = if existed {
        " Your stored login was left unchanged."
    } else {
        ""
    };
    match verdict {
        Some(false) => {
            let gone = missing(&names(rows));
            let detail = if gone.is_empty() {
                String::new()
            } else {
                format!(" (missing: {})", gone.join(", "))
            };
            return Err(format!(
                "YouTube did not accept these cookies. Make sure you copied the whole Cookie \
                 value from a request to music.youtube.com while signed in.{detail}{kept}"
            ));
        }
        None if working => {
            return Err(format!(
                "The new login was not saved because it could not be verified \
                 (no connection?).{kept}"
            ));
        }
        _ => {}
    }
    write_atomic(path, &render(rows)).map_err(|e| format!("cannot save cookies: {e}"))?;
    *CHECK.lock().unwrap() = verdict;
    Ok(if verdict == Some(true) {
        Login::Verified
    } else {
        Login::Unverified
    })
}

/// `Cookie` header value for youtube.com, `None` if there is no file or no SAPISID.
pub fn cookie_header() -> Option<String> {
    header_from(&cookie_path())
}

/// The file holds a usable-looking login; `verify` says whether YouTube agrees.
pub fn logged_in() -> bool {
    cookie_header().is_some()
}

const SESSION: [&str; 3] = ["__Secure-3PSID", "__Secure-1PSID", "SID"];
const SAPISID: [&str; 3] = ["SAPISID", "__Secure-1PAPISID", "__Secure-3PAPISID"];
/// What a complete browser login carries; used to tell the user what is missing.
const EXPECTED: [&str; 9] = [
    "SID",
    "HSID",
    "SSID",
    "APISID",
    "SAPISID",
    "__Secure-1PSID",
    "__Secure-3PSID",
    "__Secure-1PAPISID",
    "__Secure-3PAPISID",
];
const COPY: &str = "Copy the whole Cookie header of a request to music.youtube.com \
    (DevTools > Network > any 'browse' request > Request Headers > Cookie) and paste it again.";

fn names(rows: &[Row]) -> Vec<&str> {
    rows.iter()
        .filter(|r| r.is_youtube())
        .map(|r| r.name.as_str())
        .collect()
}

/// Names from `EXPECTED` that are not in `have`.
fn missing(have: &[&str]) -> Vec<&'static str> {
    EXPECTED.into_iter().filter(|n| !have.contains(n)).collect()
}

/// A session cookie and a SAPISID-family cookie, both for youtube.com.
fn complete(rows: &[Row]) -> bool {
    let have = names(rows);
    SESSION.iter().any(|n| have.contains(n)) && SAPISID.iter().any(|n| have.contains(n))
}

/// Is the stored login really signed in? `None` when YouTube could not be reached.
/// Cached after the first conclusive answer; import and logout reset it.
pub async fn signed_in() -> Option<bool> {
    if let Some(v) = *CHECK.lock().unwrap() {
        return Some(v);
    }
    verify().await
}

/// Like `signed_in` but always asks YouTube.
pub async fn verify() -> Option<bool> {
    let Some(cookie) = cookie_header() else {
        return Some(false);
    };
    let v = fetch_logged_in(&cookie).await;
    log::debug!("login check: {v:?}");
    *CHECK.lock().unwrap() = v;
    v
}

/// `verify` for code without a runtime (the `login` subcommand).
pub fn verify_blocking() -> Option<bool> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?
        .block_on(verify())
}

static CHECK: std::sync::Mutex<Option<bool>> = std::sync::Mutex::new(None);

async fn fetch_logged_in(cookie: &str) -> Option<bool> {
    // ponytail: one GET of the music.youtube.com home page, the body is only searched, never logged.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/124.0.0.0 Safari/537.36",
        )
        .build()
        .ok()?;
    let body = client
        .get("https://music.youtube.com/")
        .header("Cookie", cookie)
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    ytcfg_logged_in(&body)
}

/// `LOGGED_IN` from the ytcfg embedded in the page (plain or backslash-escaped JSON).
fn ytcfg_logged_in(html: &str) -> Option<bool> {
    let at = html.find("LOGGED_IN")?;
    let rest = html[at + "LOGGED_IN".len()..].trim_start_matches(['\\', '"', ':', ' ']);
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

pub fn logout() -> std::io::Result<()> {
    *CHECK.lock().unwrap() = None;
    match std::fs::remove_file(cookie_path()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Test helper: validate and save without asking YouTube.
#[cfg(test)]
fn import_to(path: &Path, source: &CookieSource) -> Result<(), String> {
    let rows = rows_for(source)?;
    write_atomic(path, &render(&rows)).map_err(|e| format!("cannot save cookies: {e}"))?;
    *CHECK.lock().unwrap() = None;
    Ok(())
}

/// Longest header and single value taken for a real Cookie header (they are a few KB).
const MAX_HEADER: usize = 16 * 1024;
const MAX_VALUE: usize = 4 * 1024;

/// The cookies of `source`, checked to look like a signed-in YouTube session.
fn rows_for(source: &CookieSource) -> Result<Vec<Row>, String> {
    let rows = match source {
        CookieSource::Browsers => from_browsers()?,
        CookieSource::Header(s) => {
            let rows = parse_header(s);
            if s.len() > MAX_HEADER || rows.iter().any(|r| r.value.len() > MAX_VALUE) {
                return Err("This does not look like a Cookie header.".into());
            }
            rows
        }
        CookieSource::File(p) => {
            let bytes =
                std::fs::read(p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
            let rows = String::from_utf8(bytes)
                .map(|t| parse_netscape(&t))
                .unwrap_or_default();
            if rows.is_empty() {
                return Err("This file is not a cookies.txt export.".into());
            }
            rows
        }
    };
    if !complete(&rows) {
        let gone = missing(&names(&rows));
        return Err(format!(
            "These cookies are not a signed-in YouTube session (missing: {}). {COPY}",
            gone.join(", ")
        ));
    }
    let have = names(&rows);
    if !have.contains(&"SAPISID") && !have.contains(&"__Secure-1PAPISID") {
        log::warn!("only the 3P cookie family is present, the live check decides");
    }
    Ok(rows)
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
                if complete(&rows) {
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
    // `get` because byte 7 may sit inside a multi-byte character.
    if s.get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("cookie:"))
    {
        s = &s[7..];
    }
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        + 2 * 365 * 24 * 3600;
    // Line breaks of a multi-line paste separate pairs like `;` does.
    s.split([';', '\n', '\r'])
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
    header_of(&parse_netscape(&std::fs::read_to_string(path).ok()?))
}

fn header_of(rows: &[Row]) -> Option<String> {
    let rows: Vec<&Row> = rows.iter().filter(|r| r.is_youtube()).collect();
    let find = |n: &str| rows.iter().find(|r| r.name == n).map(|r| r.value.clone());
    // The API client reads the hash input from `SAPISID=` and needs a `;` after it, so it goes
    // first. the 1P/3P APISID cookies carry the same value when SAPISID is missing.
    let sapisid = SAPISID.iter().find_map(|n| find(n))?;
    let mut seen = vec!["SAPISID"];
    let mut parts = vec![format!("SAPISID={sapisid}")];
    for r in rows {
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
        // Byte 7 is inside the `é`: no panic.
        assert_eq!(pairs(&parse_header("abcdefé=1")), [("abcdefé", "1")]);
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
            .youtube.com\tTRUE\t/\tTRUE\t0\tSID\ts\n\
            # a comment\n\
            .google.com\tTRUE\t/\tTRUE\t1900000000\tNID\tnope\n";
        let rows = parse_netscape(text);
        assert_eq!(rows.len(), 4);
        assert!(rows[0].http_only && rows[0].name == "SAPISID");
        assert!(!rows[1].http_only && !rows[1].secure);
        assert_eq!(parse_netscape(&render(&rows)), rows);

        let src = tmp("file-src");
        let dst = tmp("file-dst");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, text).unwrap();
        import_to(&dst, &CookieSource::File(src)).unwrap();
        // Only youtube.com cookies make it into the header.
        assert_eq!(
            header_from(&dst).unwrap(),
            "SAPISID=secret; PREF=f=1; SID=s"
        );
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
        assert!(err.contains("not a cookies.txt"), "{err}");
        assert!(!path.exists());
        assert_eq!(header_from(&path), None);
    }

    #[test]
    fn multi_line_paste_keeps_every_pair() {
        let rows = parse_header("cookie:\r\nSID=a;\nHSID=b\r\n__Secure-3PSID=c; PREF=f=1\n");
        assert_eq!(
            pairs(&rows),
            [
                ("SID", "a"),
                ("HSID", "b"),
                ("__Secure-3PSID", "c"),
                ("PREF", "f=1")
            ]
        );
    }

    #[test]
    fn session_rules_and_missing_names() {
        let row = |n: &str| parse_header(&format!("{n}=x")).remove(0);
        let rows = |ns: &[&str]| ns.iter().map(|n| row(n)).collect::<Vec<_>>();
        assert!(complete(&rows(&["__Secure-3PSID", "__Secure-3PAPISID"])));
        assert!(complete(&rows(&["SID", "SAPISID"])));
        assert!(!complete(&rows(&["__Secure-3PAPISID", "PREF"])));
        assert!(!complete(&rows(&["SID", "PREF"])));
        assert_eq!(
            missing(&["SID", "__Secure-3PSID", "__Secure-3PAPISID", "PREF"]),
            [
                "HSID",
                "SSID",
                "APISID",
                "SAPISID",
                "__Secure-1PSID",
                "__Secure-1PAPISID"
            ]
        );
        let path = tmp("static");
        let err = import_to(
            &path,
            &CookieSource::Header("PREF=1; __Secure-3PAPISID=y".into()),
        )
        .unwrap_err();
        assert!(
            err.contains("missing: SID, HSID") && err.contains("DevTools"),
            "{err}"
        );
        assert!(!err.contains("=y"));
        assert!(!path.exists());
    }

    #[test]
    fn a_rejected_or_unchecked_login_never_replaces_a_stored_one() {
        let good = |v: &str| parse_header(&format!("SAPISID={v}; SID=s"));
        let on_disk = |p: &Path| std::fs::read_to_string(p).unwrap();
        // First login: a rejected set is not kept, an unchecked one is.
        let path = tmp("checked");
        let e = save_checked(&path, &good("a"), Some(false)).unwrap_err();
        assert!(
            e.starts_with("YouTube did not accept these cookies."),
            "{e}"
        );
        assert!(
            e.contains("(missing: HSID") && !e.contains("left unchanged"),
            "{e}"
        );
        assert!(!path.exists());
        assert_eq!(save_checked(&path, &good("a"), None), Ok(Login::Unverified));
        // Now a login exists: neither a rejection nor "could not check" may overwrite it.
        let before = on_disk(&path);
        let e = save_checked(&path, &good("b"), Some(false)).unwrap_err();
        assert!(e.contains("left unchanged"), "{e}");
        let e = save_checked(&path, &good("b"), None).unwrap_err();
        assert!(
            e.contains("not saved") && e.contains("left unchanged"),
            "{e}"
        );
        assert_eq!(on_disk(&path), before);
        assert_eq!(
            save_checked(&path, &good("b"), Some(true)),
            Ok(Login::Verified)
        );
        assert!(on_disk(&path).contains("\tb\n"));
    }

    #[test]
    fn oversized_and_binary_input_is_refused() {
        let path = tmp("sizes");
        let big = format!("SAPISID={}; SID=s", "x".repeat(5000));
        for src in [
            CookieSource::Header(big),
            CookieSource::Header(format!("SAPISID=a; SID=b; PREF={}", "y; z=1".repeat(3000))),
        ] {
            let e = import_to(&path, &src).unwrap_err();
            assert_eq!(e, "This does not look like a Cookie header.");
        }
        let bin = tmp("bin-src");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, [0xff, 0xfe, 0, 1, 2]).unwrap();
        let e = import_to(&path, &CookieSource::File(bin)).unwrap_err();
        assert_eq!(e, "This file is not a cookies.txt export.");
        assert!(!path.exists());
    }

    #[test]
    fn ytcfg_login_flag() {
        assert_eq!(
            ytcfg_logged_in(r#"ytcfg.set({"A":1,"LOGGED_IN":true,"B":2})"#),
            Some(true)
        );
        assert_eq!(ytcfg_logged_in(r#"{"LOGGED_IN": false}"#), Some(false));
        assert_eq!(ytcfg_logged_in(r#"{\"LOGGED_IN\":true}"#), Some(true));
        assert_eq!(ytcfg_logged_in("<html>no config</html>"), None);
        assert_eq!(ytcfg_logged_in(r#""LOGGED_IN":null"#), None);
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
