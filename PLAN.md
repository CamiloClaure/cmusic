# cmusic: YouTube Music player in Rust (TUI + GUI)

Revision 2 (2026-11-10). Changes from revision 1 are listed in "What changed and why" at the bottom.

## Context
No official YouTube Music desktop app exists. Existing clients (pear-desktop, ytmdesktop, Kaset, SimpMusic, youtui, ytermusic) share the same flaws, found in their GitHub issues:
1. Playback and queue break: playback stops after N songs, songs never start, the interface drifts from what is playing.
2. Electron weight: memory leaks, CPU use while idle, slow startup.
3. A Premium account is treated as second-class: no own library, Premium-only tracks fail, likes not synced.
4. Bad login experience: pasting headers by hand, embedded-browser two-step verification loops, logged out on reboot.
5. Strict parsing: an unknown field from YouTube Music crashes the app.
6. OS media-control regressions (media keys, Windows taskbar buttons, macOS Now Playing duplicates).
7. Packaging gaps (Flatpak, Homebrew cask, winget, ARM) and updates that block startup.
8. Missing features: volume normalization, gapless, remembering settings, audio-only, good synced lyrics.

Goal: one lightweight Rust codebase with a terminal app and a native desktop app for Windows, Linux and macOS, fixing items 1-8 by design. Repo `github.com/CamiloClaure/cmusic`, GPL-3.0.

Decisions made (unchanged): iced for the GUI, libmpv for audio, GPL-3.0, name `cmusic`.

## Architecture
```
cmusic/                      cargo workspace, lives in ~/personal/cmusic
  crates/core/   lib `cmusic-core`: types, engine, player (reducer + mpv), queue, resolve, api, auth, media, persist, lyrics
  crates/tui/    bin `cmusic`      ratatui 0.30 + crossterm
  crates/gui/    bin `cmusic-gui`  iced 0.14
```
Pinned crates (checked on crates.io 2026-11-10): `ytmapi-rs` 0.3, `libmpv2` 7 (no default features, the `render` feature is video only), `souvlaki` 0.9 with `use_zbus` (no libdbus build dependency), `rookie` 0.5, `ratatui` 0.30, `iced` 0.14, `directories` 6, `tokio`, `serde`/`serde_json`, `reqwest` (comes with ytmapi-rs).

- **Catalog:** `ytmapi-rs` (search, library, playlists, likes, radio, lyrics).
- **Streams:** our own resolver runs `yt-dlp -j -f bestaudio/best --no-playlist [--cookies <file>] https://music.youtube.com/watch?v=<id>` and reads `url` and `http_headers` from the JSON. mpv gets the direct URL plus those headers (without the headers googlevideo answers 403, verified). This replaces mpv's `ytdl_hook`, which needs a Lua-enabled libmpv and is missing in several builds (conda-forge for one). Bonus: yt-dlp's stderr becomes the error message shown to the user. Runtime needs `yt-dlp` and `deno` on PATH.
  - URLs expire in about 6 hours, so only the current and the next track are ever resolved, just in time. A restored session re-resolves.
- **Audio:** `libmpv2`. `video=no`, `gapless-audio=yes`, `prefetch-playlist=yes`, `stop-screensaver=no`, `audio-display=no`. mpv's playlist holds at most two entries (current and next). Any queue edit that changes "next" removes and re-appends it, so mpv can never drift from our queue.
- **One contract for both front ends** (`core/src/types.rs`, written before any worker starts):
  - `engine::start() -> (Handle, UnboundedReceiver<Update>)`. `Handle::send(Command)` is the only way in.
  - `Command`: playback (`PlayTracks`, `Enqueue`, `PlayNext`, `JumpTo`, `RemoveFromQueue`, `ClearQueue`, `TogglePause`, `Next`, `Prev`, `SeekTo`, `SeekBy`, `SetVolume`, `ToggleShuffle`, `CycleRepeat`, `ToggleNormalize`), browsing (`Fetch(PageRequest)`, `Rate`, `AddToPlaylist`, `RemoveFromPlaylist`), login (`ImportCookies`), `Quit`.
  - `Update`: `Player(Snapshot)`, `Page{request, result}`, `Lyrics`, `AuthRequired`, `Notice`.
  - Front ends hold no playback logic and never call the API or mpv directly.
- **Engine** (`core/src/engine.rs`): one thread with a tokio runtime. It owns the reducer state, feeds it events (commands, mpv events, resolver results), runs the effects, and emits a `Snapshot` after every change. Position ticks are throttled to 1 per second.
- Skipped: a background daemon so the terminal and desktop apps share one player. Add if running both at once becomes a real need.

## Steps

### 0. Toolchain (done, root-free, WSL Ubuntu 20.04)
- No sudo on the box and apt only has mpv 0.32, too old for `libmpv2` 7. Installed instead: rustup stable (cargo 1.99), micromamba env `cmusic` from conda-forge (mpv 0.42 with `libmpv.so.2`), `yt-dlp` and `deno` binaries in `~/.local/bin`.
- `source ~/.local/cmusic-env.sh` before any cargo command (sets PATH and the `-L` / rpath flags for libmpv).
- Verified: a `libmpv2` probe links and runs; `yt-dlp -j` plus headers plays 3 s through mpv with `ao=null`.

### 1. Repo bootstrap
- Workspace with the 3 crates, all dependencies declared up front (workers do not touch `Cargo.toml` without saying so), `types.rs` contract, GPL-3.0 `LICENSE`, short `README.md` (goal, status, unofficial, own account needed, runtime needs), `.gitignore`, `rustfmt.toml`.
- CI `.github/workflows/ci.yml`: fmt, clippy `-D warnings`, test. Ubuntu job installs `libmpv-dev`; Windows and macOS jobs install mpv dev files (shinchiro libmpv zip, `brew install mpv`).
- `git init -b main`, local commits. **The public `gh repo create ... --push` happens only after Camilo confirms**, once the smoke test with the real account passes. Until then "CI green" is unverified.

### 2. Core: player state machine and queue (fixes flaw 1)
- `player/state.rs`: pure `reduce(State, Event) -> (State, Vec<Effect>)`, no IO, no time.
  - Status: `Idle | Loading | Playing | Paused | Buffering | Error`.
  - Events: commands, resolver results (`Resolved{video_id, stream}`, `ResolveFailed{video_id, message}`), mpv events (`FileLoaded`, `EndFile{Eof | Error | Stopped}`, `PauseChanged`, `TimePos`, `Duration`, `Buffering`), `RadioFetched(Vec<Track>)`.
  - Effects: `Resolve(video_id)`, `Load(stream)`, `AppendNext(stream)`, `ClearNext`, `SetPause`, `Seek`, `SetVolume`, `SetNormalize`, `Stop`, `FetchRadio(video_id)`, `Persist`.
  - Stale results are dropped: a `Resolved` for a track that is no longer current or next is ignored (the classic "wrong song plays after fast skipping" bug).
  - Failure policy: a failed track records `last_error` and skips forward. Three failures in a row stop in `Error` instead of burning through the queue (offline, or cookies expired).
- `queue.rs`: track list, cursor, shuffle order kept apart from the original order, repeat modes, autoplay. When 2 or fewer tracks remain and repeat is off, emit `FetchRadio`. The queue never runs dry.
- Table-driven tests, no mpv: end of track, gapless advance, last track with autoplay, failure skip, the 3-failure stop, stale resolve, pause/resume, repeat-one, repeat-all, shuffle on/off keeps the current track, remove before/at/after the cursor.

### 3. Core: resolver and mpv adapter
- `resolve.rs`: the yt-dlp call above. Async (`tokio::process`), 30 s timeout, last stderr line as the error. A fixture test parses a saved JSON; an `#[ignore]` test hits the network.
- `player/mpv.rs`: thin adapter. Methods mirror the effects; events come out through a callback from one event thread. Headers go per file through `loadfile` options. Normalization is `af=loudnorm` toggled at runtime. An `#[ignore]` test plays 3 s of a resolved stream with `ao=null` and asserts `FileLoaded` then `TimePos`.

### 4. Core: login (fixes flaw 4)
- `auth.rs`: one Netscape `cookies.txt` in the config folder, owner-only (0600 on Unix), is the single credential store. yt-dlp reads it directly and the `Cookie` header for ytmapi-rs is derived from it.
- Import order: `rookie` over Firefox, Chrome, Edge, Brave (first one that has `SAPISID` for `.youtube.com` wins). Fallbacks: paste a `Cookie` header, or point at an exported `cookies.txt`.
- Known limit, stated in the README: on Windows, Chrome-family browsers since v127 encrypt cookies per app, so automatic import there is Firefox only. Chrome users on Windows use the paste or file fallback.
- A 401/403 from the API emits `Update::AuthRequired`. Both interfaces show a re-import prompt, never a crash.
- On WSL the browsers are Windows-side, so this box tests with the paste or file fallback.

### 5. Core: API layer (fixes flaws 3 and 5)
- `api.rs` wraps `ytmapi-rs`: search, library (playlists, songs, albums, artists, liked songs), playlist/album/artist pages with continuations followed to the end, like/dislike, add to and remove from playlist, radio (`get_watch_playlist`).
- Everything maps to our own `Track` / `Item` / `Page` types, so a ytmapi-rs change touches one file.
- Tolerant by construction at our layer: every call returns `Result<_, String>`, an error becomes a banner, an item that cannot be mapped is skipped and logged. Parsing bugs inside ytmapi-rs go upstream as pull requests.
- No local favorites: likes and playlists live only in the account.
- Works logged out too (search and play), so the app is usable before login.

### 6. Core: extras (fixes flaw 8)
- `persist.rs`: queue, cursor, position, volume, shuffle, repeat, normalization as `state.json` in the platform state folder (`directories`). Written atomically (temp file, then rename), debounced by the engine. A corrupt file is ignored, never fatal. Restore comes back paused.
- `lyrics.rs`: synced lyrics from LRCLIB first (`/api/get` by title, artist, duration), plain lyrics from YouTube Music as fallback, cached on disk by video id. LRC parser with tests.

### 7. Core: OS media controls (fixes flaw 6)
- `media.rs`: `souvlaki` (MPRIS, SMTC, Now Playing). Fed from `Snapshot` only, and only when track, status or duration changed, so one entry and no duplicates. Buttons map straight to `Command`s.
- Windows needs a window handle: the GUI passes its own. The TUI on Windows has no media keys in v1.

### 8. Engine
- `engine.rs` wires steps 2-7 as described in Architecture. Lyrics are fetched on track change and pushed as `Update::Lyrics`. Saves on `Persist` effects and on `Quit`.
- Test: the engine runs against a fake player and fake resolver (the only trait in the project, it exists for this test) and plays through a 3-track queue.
- `cargo run -p cmusic-core --example play -- <video_id>` plays one track headless. This is the end-to-end check that works on this box.

### 9. TUI (`cmusic`)
- Panes: Library | Search | Queue | Now Playing with synced lyrics.
- Vim and arrow keys, fixed in v1. `?` shows them.
- Redraws only on an `Update` or a key press. The 1 s progress tick is the engine's snapshot, so idle CPU stays near 0 when paused.
- One Ctrl-C (or `q`) restores the terminal and exits, also on panic.
- `cmusic login [--paste | --file <cookies.txt>]`.

### 10. GUI (`cmusic-gui`)
- iced, with a subscription over the `Update` receiver.
- Screens: Library, Search, Album/Playlist/Artist page, Queue sidebar, Now Playing bar, lyrics view. Login dialog on `AuthRequired`.
- Album art loaded lazily with a disk cache.
- Window size persisted.
- Targets: about 0 % CPU when paused, under 150 MB.

### 11. After the account smoke test (not built in this pass)
- Public repo push and CI on 3 OSes.
- Packaging (fixes flaw 7): GitHub Releases through `cargo-dist` (Windows msi with `libmpv-2.dll`, `yt-dlp.exe` and `deno.exe` bundled; macOS universal; Linux x86_64 and aarch64), Homebrew tap, Flatpak, winget. Update check is an async notice, never blocking.
- Windows taskbar thumbnail buttons (`ITaskbarList3`), configurable keybindings, `cmusic play <query>`.
- Later, on request: Chromecast, equalizer, offline downloads, Discord and Last.fm.

## Tasks and orchestration
Fable orchestrates and reviews. Workers are lower models, each in its own git worktree and branch, with disjoint files so merges are clean.

| # | Task | Files | Model | Depends on |
|---|------|-------|-------|-----------|
| T0 | Scaffold, contract, CI, docs | workspace, `types.rs` | Fable | - |
| T1 | Reducer and queue | `player/state.rs`, `queue.rs` | Opus | T0 |
| T2 | Resolver and mpv adapter | `resolve.rs`, `player/mpv.rs` | Sonnet | T0 |
| T3 | Auth and API | `auth.rs`, `api.rs` | Sonnet | T0 |
| T4 | Persist, lyrics, media controls | `persist.rs`, `lyrics.rs`, `media.rs` | Sonnet | T0 |
| T5 | Engine and `play` example | `engine.rs`, `examples/play.rs` | Opus | T1-T4 |
| T6 | TUI | `crates/tui` | Sonnet | T5 |
| T7 | GUI | `crates/gui` | Sonnet | T5 |
| T8 | Review, fix, final verification | all | Fable + reviewer | T6, T7 |

T1-T4 run in parallel, then T5, then T6 and T7 in parallel.

## Verification
- Every task: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` in its worktree, and again on `main` after each merge.
- T1: the listed reducer cases pass.
- T2: the `#[ignore]` network tests pass on this box (`cargo test -- --ignored`).
- T5: the fake-backed engine test passes, and the `play` example plays a public track headless.
- T6/T7: both binaries build and start. The TUI is driven in a pseudo-terminal for a search, play, quit pass. The GUI is launched under WSLg if a display is available.
- Left for Camilo, with the real Premium account (cannot be automated here):
  - `cmusic login`, then search, play, queue 30+ tracks and let autoplay run.
  - Play a Premium-only track.
  - Like a song and see it on music.youtube.com.
  - Media keys and a single OS media entry on Windows, Linux and macOS.
  - Idle CPU when paused; restart restores queue and position.

## What changed and why
1. **Own yt-dlp resolver instead of mpv's `ytdl_hook`.** The hook needs Lua inside libmpv. The conda-forge build on this box has none, and it would be a silent packaging trap elsewhere. Costs about 40 lines, gives real error messages.
2. **No OS keyring.** yt-dlp needs the cookies as a file on disk anyway, so the keyring protected nothing and added a failure mode (no secret service on headless Linux and WSL). One 0600 file.
3. **Honest login scope.** `rookie` was last released in 2024 and cannot read Chrome-family cookies on Windows any more. Firefox works everywhere; paste and file import are first-class fallbacks.
4. **The Command / Update contract is written first.** Revision 1 named `Command` and `Snapshot` without defining them. It is what lets the terminal and desktop apps be built in parallel and stay identical.
5. **Reducer hardening:** stale-resolve drop, a 3-failure stop, and the two-entry mpv playlist rule. These are the exact causes of flaw 1 in the other clients.
6. **Toolchain reality:** no sudo and Ubuntu 20.04, so libmpv comes from conda-forge. `souvlaki` uses zbus so no libdbus headers are needed.
7. **Public push and packaging moved after the real-account smoke test.** Nothing unverified goes public, and installers for an app that has not played a Premium track yet are wasted work.
8. **Cut from v1:** configurable keybindings, Windows taskbar buttons, TOML (state is JSON, nobody hand-edits it). Each returns when asked for.
