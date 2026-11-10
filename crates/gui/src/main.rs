mod app;
mod theme;
mod view;

use app::{Message, Out, State};
use cmusic_core::engine::{self, Handle, Options};
use cmusic_core::{persist, Update};
use iced::futures::stream;
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
        (s, Task::none())
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
        // Status::Ignored means no text input took the key.
        event::listen_with(|e, status, _| match e {
            iced::Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Space),
                ..
            }) if status == event::Status::Ignored => Some(Message::TogglePause),
            iced::Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Character(c),
                modifiers,
                ..
            }) if modifiers.control() && c.as_str() == "r" => Some(Message::ReloadTheme),
            _ => None,
        }),
        window::resize_events().map(|(_, s)| Message::Resized(s)),
        window::close_requests().map(|_| Message::CloseRequested),
    ])
}

fn update(app: &mut State, msg: Message) -> Task<Message> {
    let Out {
        commands,
        art,
        closing,
        stopped,
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
    if closing {
        save_prefs(app.size, &app.theme_name);
        // Wait for the engine to save and stop, but not forever.
        tasks.push(Task::future(tokio::time::sleep(Duration::from_secs(3))).then(|_| iced::exit()));
    }
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
        return Some(b);
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
    Some(bytes.to_vec())
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
