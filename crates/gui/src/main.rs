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
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;

/// The receiver is moved into the subscription stream exactly once. iced identifies a
/// `Subscription::run` by its fn pointer, so the stream is built once and kept alive.
static RX: Mutex<Option<UnboundedReceiver<Update>>> = Mutex::new(None);
static HANDLE: OnceLock<Handle> = OnceLock::new();
static HTTP: OnceLock<reqwest::Client> = OnceLock::new();

const DEFAULT_SIZE: Size = Size::new(1100.0, 700.0);

fn main() -> iced::Result {
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
        s.logged_in = cmusic_core::auth::logged_in();
        (s, demo_start())
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
    let follow = demo_followups(&msg);
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
            Task::future(tokio::time::sleep(Duration::from_secs(5)))
                .map(move |_| Message::ClearNotice(g)),
        );
    }
    if closing {
        save_prefs(app.size, &app.theme_name);
        // Wait for the engine to save and stop, but not forever.
        tasks.push(Task::future(tokio::time::sleep(Duration::from_secs(3))).then(|_| iced::exit()));
    }
    tasks.extend(follow.into_iter().map(|m| update(app, m)));
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

// Debug harness for screenshots, inert unless these env vars are set:
//   CMUSIC_DEMO=<query>        search it at startup, then play row 0 when the page arrives
//   CMUSIC_SHOT=<path.png>     after CMUSIC_SHOT_AFTER secs (default 20) save the window, then exit
//   CMUSIC_DEMO_VIEW=lyrics|login  switch the main area to that view when row 0 starts playing
fn demo_start() -> Task<Message> {
    let mut t = Task::none();
    if let Ok(q) = std::env::var("CMUSIC_DEMO") {
        t = Task::done(Message::SearchInput(q)).chain(Task::done(Message::SearchSubmit));
    }
    if let Ok(path) = std::env::var("CMUSIC_SHOT") {
        let secs = std::env::var("CMUSIC_SHOT_AFTER")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20);
        let shot = Task::future(tokio::time::sleep(Duration::from_secs(secs)))
            .then(|_| window::latest())
            .and_then(window::screenshot)
            .map(move |s| {
                if let Some(img) =
                    ::image::RgbaImage::from_raw(s.size.width, s.size.height, s.rgba.to_vec())
                {
                    let _ = img.save(&path);
                }
                std::process::exit(0)
            });
        t = t.chain(shot);
    }
    t
}

/// Once the demo search page lands: play row 0 (and switch view), as the UI would.
fn demo_followups(msg: &Message) -> Vec<Message> {
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !matches!(msg, Message::Engine(Update::Page { .. }))
        || std::env::var_os("CMUSIC_DEMO").is_none()
        || DONE.swap(true, std::sync::atomic::Ordering::Relaxed)
    {
        return vec![];
    }
    let mut v = vec![Message::PlayRow(0)];
    match std::env::var("CMUSIC_DEMO_VIEW").as_deref() {
        Ok("lyrics") => v.push(Message::ToggleLyrics),
        Ok("login") => v.push(Message::OpenLogin),
        _ => {}
    }
    v
}
