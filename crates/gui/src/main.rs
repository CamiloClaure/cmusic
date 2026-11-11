mod app;
mod icons;
mod theme;
mod view;

use app::{Message, Out, Scroll, State};
use cmusic_core::engine::{self, Handle, Options};
use cmusic_core::{persist, Update};
use iced::futures::stream;
use iced::widget::operation;
use iced::{event, keyboard, window, Size, Subscription, Task};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;

/// The receiver is moved into the subscription stream exactly once. iced identifies a
/// `Subscription::run` by its fn pointer, so the stream is built once and kept alive.
static RX: Mutex<Option<UnboundedReceiver<Update>>> = Mutex::new(None);
static HANDLE: OnceLock<Handle> = OnceLock::new();
static HTTP: OnceLock<reqwest::Client> = OnceLock::new();

const DEFAULT_SIZE: Size = Size::new(1100.0, 700.0);

/// Under WSLg the Wayland compositor (weston) can stop answering while X11 keeps working, and
/// winit then waits forever for its first reply. So prefer X11 there; `CMUSIC_WAYLAND=1` opts out.
fn prefer_x11_on_wsl() {
    let wsl = std::env::var_os("WSL_DISTRO_NAME").is_some()
        || std::path::Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists();
    if wsl && std::env::var_os("DISPLAY").is_some() && std::env::var_os("CMUSIC_WAYLAND").is_none()
    {
        // First thing in `main`, before any thread exists (set/remove_var is not thread safe).
        std::env::remove_var("WAYLAND_DISPLAY");
    }
}

fn main() -> iced::Result {
    prefer_x11_on_wsl();
    // ponytail: hwnd stays None so Windows media controls are off; pass the window handle here.
    match engine::start(Options { hwnd: None }) {
        Ok((handle, rx)) => {
            let _ = HANDLE.set(handle);
            *RX.lock().unwrap() = Some(rx);
        }
        Err(e) => {
            eprintln!("cmusic-gui: cannot start the engine: {e}");
            std::process::exit(1);
        }
    }
    theme::install_presets();
    let (size, saved) = load_prefs();
    let (look, theme_name, notice) = match theme::load(&saved) {
        Ok(l) => (l, saved, None),
        Err(e) => (theme::Look::default(), theme::DEFAULT.to_string(), Some(e)),
    };
    let init = move || {
        let mut s = State::new(size);
        s.look = look;
        s.theme_name = theme_name.clone();
        s.notice = notice.clone();
        // Unknown until the engine's first `Update::Session` (it checks a stored login live).
        s.logged_in = false;
        // A theme error found at startup is generation 0; it goes away like any notice.
        let gone = Task::future(tokio::time::sleep(Duration::from_secs(15)))
            .map(|_| Message::ClearNotice(0));
        (s, Task::batch([demo_start(), gone]))
    };
    iced::application(init, update, view::view)
        .title("cmusic")
        .theme(|s: &State| view::theme(s.look))
        .subscription(subscription)
        .window(window::Settings {
            size,
            min_size: Some(Size::new(800.0, 500.0)),
            exit_on_close_request: false,
            ..Default::default()
        })
        .run()
}

fn engine_stream() -> impl iced::futures::Stream<Item = Message> {
    let rx = RX.lock().unwrap().take();
    stream::unfold(rx, |rx| async {
        let mut rx = rx?;
        match rx.recv().await {
            Some(u) => Some((Message::Engine(u), Some(rx))),
            None => Some((Message::EngineStopped, None)),
        }
    })
}

fn subscription(_: &State) -> Subscription<Message> {
    Subscription::batch([
        Subscription::run(engine_stream),
        event::listen_with(|e, status, _| {
            let iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = e
            else {
                return None;
            };
            use keyboard::key::Named::*;
            use keyboard::Key::{Character, Named};
            let ctrl = modifiers.control();
            match key.as_ref() {
                // Work even while a text input has focus.
                Character("f") if ctrl => Some(Message::FocusSearch),
                Character("r") if ctrl => Some(Message::ReloadTheme),
                Character("t") if ctrl => Some(Message::CycleTheme),
                Character("l") if ctrl => Some(Message::ToggleLyrics),
                // Status::Ignored means no text input took the key.
                _ if status != event::Status::Ignored => None,
                Character("/") => Some(Message::FocusSearch),
                Named(Space) => Some(Message::TogglePause),
                Named(Escape) => Some(Message::Escape),
                Named(ArrowLeft) if ctrl => Some(Message::Prev),
                Named(ArrowRight) if ctrl => Some(Message::Next),
                Named(ArrowLeft) => Some(Message::SeekBy(-5.0)),
                Named(ArrowRight) => Some(Message::SeekBy(5.0)),
                Named(ArrowUp) => Some(Message::VolumeBy(5)),
                Named(ArrowDown) => Some(Message::VolumeBy(-5)),
                _ => None,
            }
        }),
        window::resize_events().map(|(_, s)| Message::Resized(s)),
        window::close_requests().map(|_| Message::CloseRequested),
    ])
}

fn update(app: &mut State, msg: Message) -> Task<Message> {
    if let Message::Demo(t) = msg {
        return demo_step(app, t);
    }
    // A `waitpage` in progress ends when the next page arrives.
    let follow = if matches!(msg, Message::Engine(Update::Page { .. }))
        && WAIT.swap(0, Ordering::Relaxed) != 0
    {
        Some(Message::Demo(None))
    } else {
        None
    };
    app.now = std::time::Instant::now();
    let Out {
        commands,
        art,
        closing,
        stopped,
        focus_search,
        scroll,
        notice_gen,
    } = app.update(msg);
    if let Some(h) = HANDLE.get() {
        for c in commands {
            h.send(c);
        }
    }
    if stopped {
        save_prefs(app.size, &app.theme_name);
        return iced::exit();
    }
    let mut tasks: Vec<Task<Message>> = art
        .into_iter()
        .map(|url| {
            Task::perform(fetch_art(url.clone()), move |b| {
                Message::Art(url.clone(), b)
            })
        })
        .collect();
    if focus_search {
        tasks.push(operation::focus(app::SEARCH));
    }
    match scroll {
        Some(Scroll::ListTop) => {
            tasks.push(operation::snap_to(
                app::LIST,
                operation::RelativeOffset::START,
            ));
        }
        Some(Scroll::Lyrics(y)) => tasks.push(operation::scroll_to(
            app::LYRICS,
            operation::AbsoluteOffset {
                x: None,
                y: Some(y),
            },
        )),
        None => {}
    }
    if let Some(g) = notice_gen {
        tasks.push(
            Task::future(tokio::time::sleep(Duration::from_secs(app.notice_secs)))
                .map(move |_| Message::ClearNotice(g)),
        );
    }
    if closing {
        save_prefs(app.size, &app.theme_name);
        // Wait for the engine to save and stop, but not forever.
        tasks.push(Task::future(tokio::time::sleep(Duration::from_secs(3))).then(|_| iced::exit()));
    }
    tasks.extend(follow.map(Task::done));
    Task::batch(tasks)
}

/// Disk cache first, then download. Failure just leaves the placeholder.
async fn fetch_art(url: String) -> Option<Vec<u8>> {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h = DefaultHasher::new();
    url.hash(&mut h);
    let path = persist::cache_dir()
        .join("art")
        .join(format!("{:016x}", h.finish()));
    if let Ok(b) = tokio::fs::read(&path).await {
        return tokio::task::spawn_blocking(|| app::trim_bars(b)).await.ok();
    }
    let client = HTTP.get_or_init(reqwest::Client::new);
    let bytes = client
        .get(&url)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .bytes()
        .await
        .ok()?;
    if let Some(dir) = path.parent() {
        let _ = tokio::fs::create_dir_all(dir).await;
    }
    let _ = tokio::fs::write(&path, &bytes).await;
    tokio::task::spawn_blocking(move || app::trim_bars(bytes.to_vec()))
        .await
        .ok()
}

fn prefs_path() -> std::path::PathBuf {
    persist::state_path().with_file_name("gui.json")
}

/// Window size and theme name; a missing file or key gives the defaults.
fn load_prefs() -> (Size, String) {
    let v: Option<serde_json::Value> = std::fs::read_to_string(prefs_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());
    let get = |k: &str| v.as_ref()?.get(k)?.as_f64();
    let size = match (get("w"), get("h")) {
        (Some(w), Some(h)) if w >= 800.0 && h >= 500.0 => Size::new(w as f32, h as f32),
        _ => DEFAULT_SIZE,
    };
    let theme = v
        .as_ref()
        .and_then(|v| v.get("theme")?.as_str())
        .unwrap_or(theme::DEFAULT);
    (size, theme.to_string())
}

fn save_prefs(s: Size, theme: &str) {
    let _ = std::fs::write(
        prefs_path(),
        serde_json::json!({"w": s.width, "h": s.height, "theme": theme}).to_string(),
    );
}

// Debug harness, inert unless CMUSIC_DEMO_SCRIPT="step;step;..." is set. Steps run in order:
//   search:<q>  waitpage (next Page update, max 30 s)  play:<row>  wait:<secs>
//   seek:<secs>  seekfrac:<0..1>  pause  next  prev  lyrics  login  theme
//   open:<liked|playlists|songs|albums|artists>  state  shot:<path.png>  quit
// `seek` goes through SeekChanged then SeekReleased, like a drag. `state` prints one line to stderr.
static SCRIPT: Mutex<Option<VecDeque<String>>> = Mutex::new(None);
/// Generation of the `waitpage` in progress, 0 when none.
static WAIT: AtomicU64 = AtomicU64::new(0);

fn demo_start() -> Task<Message> {
    match std::env::var("CMUSIC_DEMO_SCRIPT") {
        Ok(s) => {
            let steps = s
                .split(';')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty());
            *SCRIPT.lock().unwrap() = Some(steps.collect());
            Task::done(Message::Demo(None))
        }
        Err(_) => Task::none(),
    }
}

/// Handles `Message::Demo`: runs the next step, which hands back to `Demo` when it is done.
fn demo_step(app: &State, timeout: Option<u64>) -> Task<Message> {
    use cmusic_core::PageRequest as P;
    if let Some(g) = timeout {
        // Only the timer of the wait still in progress may continue the script.
        if WAIT
            .compare_exchange(g, 0, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return Task::none();
        }
        eprintln!("demo: waitpage timed out");
    }
    let Some(step) = SCRIPT.lock().unwrap().as_mut().and_then(|q| q.pop_front()) else {
        return Task::none();
    };
    let (name, arg) = step.split_once(':').unwrap_or((step.as_str(), ""));
    let next = || Task::done(Message::Demo(None));
    let run = |msgs: Vec<Message>| {
        msgs.into_iter()
            .fold(Task::none(), |t, m| t.chain(Task::done(m)))
            .chain(next())
    };
    match name {
        "search" => run(vec![
            Message::SearchInput(arg.into()),
            Message::SearchSubmit,
        ]),
        "waitpage" => {
            let g = WAIT.load(Ordering::Relaxed) + 1;
            WAIT.store(g, Ordering::Relaxed);
            Task::future(tokio::time::sleep(Duration::from_secs(30)))
                .map(move |_| Message::Demo(Some(g)))
        }
        "play" => run(vec![Message::PlayRow(arg.parse().unwrap_or(0))]),
        "wait" => Task::future(tokio::time::sleep(Duration::from_secs_f64(
            arg.parse().unwrap_or(1.0),
        )))
        .map(|_| Message::Demo(None)),
        "seek" | "seekfrac" => {
            let v: f64 = arg.parse().unwrap_or(0.0);
            let secs = if name == "seek" {
                v
            } else {
                v * app.snapshot.duration_secs
            };
            run(vec![Message::SeekChanged(secs), Message::SeekReleased])
        }
        "pause" => run(vec![Message::TogglePause]),
        "next" => run(vec![Message::Next]),
        "prev" => run(vec![Message::Prev]),
        "lyrics" => run(vec![Message::ToggleLyrics]),
        "login" => run(vec![Message::OpenLogin]),
        "theme" => run(vec![Message::CycleTheme]),
        "open" => {
            let r = match arg {
                "liked" => P::LikedSongs,
                "playlists" => P::LibraryPlaylists,
                "songs" => P::LibrarySongs,
                "albums" => P::LibraryAlbums,
                _ => P::LibraryArtists,
            };
            run(vec![Message::Open(r)])
        }
        "state" => {
            let view = if app.login.is_some() {
                "login"
            } else if app.show_lyrics {
                "lyrics"
            } else {
                "list"
            };
            let page = match (&app.failed, &app.current) {
                (Some((_, e)), _) => format!("error {e}"),
                (_, Some((_, p))) => format!("{} items", p.items.len()),
                _ if app.awaiting.is_some() => "loading".into(),
                _ => "none".into(),
            };
            let s = &app.snapshot;
            eprintln!(
                "state: status={:?} pos={:.1} dur={:.1} view={view} page={page} logged_in={} notice={:?}",
                s.status, s.position_secs, s.duration_secs, app.logged_in, app.notice
            );
            next()
        }
        "shot" => {
            let path = arg.to_string();
            window::latest().and_then(window::screenshot).map(move |s| {
                if let Some(img) =
                    ::image::RgbaImage::from_raw(s.size.width, s.size.height, s.rgba.to_vec())
                {
                    let _ = img.save(&path);
                }
                Message::Demo(None)
            })
        }
        "quit" => Task::done(Message::CloseRequested),
        _ => {
            eprintln!("demo: unknown step {step:?}");
            next()
        }
    }
}
