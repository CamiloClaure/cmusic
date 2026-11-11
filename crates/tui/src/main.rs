mod app;
mod ui;

use app::App;
use cmusic_core::engine::{start, Options};
use cmusic_core::{auth, Command, CookieSource, Update};
use ratatui::crossterm::event::{self, Event, KeyEvent, KeyEventKind};
use std::io::Write;
use std::sync::mpsc;
use std::time::{Duration, Instant};

enum AppEvent {
    Key(KeyEvent),
    Redraw,
    Update(Update),
    EngineStopped,
}

const HELP: &str = "cmusic: YouTube Music in the terminal

usage:
  cmusic                      start the player (press ? for keys)
  cmusic login                import cookies from your browsers
  cmusic login --paste        read a Cookie header from stdin
  cmusic login --file <path>  import a Netscape cookies.txt
  cmusic logout               delete the saved login
  cmusic --help | --version";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => run(),
        ["--help" | "-h"] => {
            println!("{HELP}");
            0
        }
        ["--version" | "-V"] => {
            println!("cmusic {}", env!("CARGO_PKG_VERSION"));
            0
        }
        ["login", rest @ ..] => login(rest),
        ["logout"] => logout(),
        _ => {
            eprintln!("{HELP}");
            2
        }
    };
    std::process::exit(code);
}

fn login(args: &[&str]) -> i32 {
    let source = match args {
        [] => CookieSource::Browsers,
        ["--paste"] => {
            eprint!("Paste the Cookie header value, then Enter: ");
            let _ = std::io::stderr().flush();
            let mut s = String::new();
            let stdin = std::io::stdin();
            // A pipe may hold a multi-line paste; a terminal ends at the first Enter.
            let read = if std::io::IsTerminal::is_terminal(&stdin) {
                stdin.read_line(&mut s)
            } else {
                std::io::Read::read_to_string(&mut stdin.lock(), &mut s)
            };
            if read.is_err() {
                eprintln!("could not read stdin");
                return 1;
            }
            CookieSource::Header(s.trim().to_string())
        }
        ["--file", path] => CookieSource::File(path.into()),
        _ => {
            eprintln!("{HELP}");
            return 2;
        }
    };
    match auth::import_verified_blocking(source) {
        Ok(auth::Login::Verified) => {
            println!(
                "Logged in (verified with YouTube). Cookies saved to {}",
                auth::cookie_path().display()
            );
            0
        }
        Ok(auth::Login::Unverified) => {
            eprintln!("Cookies saved, but the login could not be verified (no connection?)");
            1
        }
        Err(e) => {
            eprintln!("login failed: {e}");
            1
        }
    }
}

fn logout() -> i32 {
    match auth::logout() {
        Ok(()) => {
            println!("Logged out: the saved cookies are deleted");
            0
        }
        Err(e) => {
            eprintln!("could not log out: {e}");
            1
        }
    }
}

fn run() -> i32 {
    let (handle, mut updates) = match start(Options::default()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("cmusic: {e}");
            return 1;
        }
    };
    let (tx, rx) = mpsc::channel();
    let tx2 = tx.clone();
    std::thread::spawn(move || {
        while let Some(u) = updates.blocking_recv() {
            if tx2.send(AppEvent::Update(u)).is_err() {
                return;
            }
        }
        let _ = tx2.send(AppEvent::EngineStopped);
    });
    // The first draw comes from the engine's initial Player update; this one shows the layout
    // right away. Raw mode must be on before the reader thread starts.
    let mut terminal = match ratatui::try_init() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cmusic: cannot start the terminal: {e}");
            handle.send(Command::Quit);
            return 1;
        }
    };
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            let ev = match ev {
                Event::Key(k) if k.kind != KeyEventKind::Release => AppEvent::Key(k),
                Event::Resize(..) => AppEvent::Redraw,
                _ => continue,
            };
            if tx.send(ev).is_err() {
                return;
            }
        }
    });

    let mut app = App::default();
    let mut stopped = false;
    'main: loop {
        if let Err(e) = terminal.draw(|f| ui::draw(&app, f)) {
            ratatui::restore();
            eprintln!("cmusic: draw failed: {e}");
            handle.send(Command::Quit);
            return 1;
        }
        let Ok(first) = rx.recv() else { break };
        // Handle everything already queued before the next draw.
        for ev in std::iter::once(first).chain(rx.try_iter()) {
            match ev {
                AppEvent::Key(k) => app.on_key(k).into_iter().for_each(|c| handle.send(c)),
                AppEvent::Update(u) => app.on_update(u).into_iter().for_each(|c| handle.send(c)),
                AppEvent::Redraw => {}
                AppEvent::EngineStopped => {
                    stopped = true;
                    break 'main;
                }
            }
            if app.quit {
                break 'main;
            }
        }
    }
    ratatui::restore();
    if !stopped {
        // Quit was sent by on_key; wait (bounded) for the engine to save and stop mpv.
        let deadline = Instant::now() + Duration::from_secs(3);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            if matches!(rx.recv_timeout(left), Ok(AppEvent::EngineStopped) | Err(_)) {
                break;
            }
        }
    }
    0
}
