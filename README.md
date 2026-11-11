# cmusic

Lightweight YouTube Music player written in Rust: a terminal app (`cmusic`) and a native desktop app (`cmusic-gui`) over one shared core.

Status: early development, not released. Plays end to end on Linux; account features and other platforms are untested.

Unofficial. Not affiliated with Google or YouTube. You need your own YouTube Music account, and a Premium account for Premium-only tracks.

## Why
Existing clients break playback after a few songs, eat memory (Electron), treat the account library as an afterthought, and crash when YouTube changes a field. cmusic keeps all playback logic in one pure state machine, plays through libmpv, and reads your library, likes and playlists straight from your account.

## Runtime needs
- `libmpv` 0.38 or newer
- `yt-dlp` and `deno` on PATH (stream resolving)

## Login
`cmusic login` imports the YouTube cookies from your browser. Firefox works everywhere. Chrome, Edge and Brave work on Linux and macOS; on Windows they encrypt cookies per app, so use `cmusic login --paste` (paste the `Cookie` header from the browser dev tools) or `cmusic login --file cookies.txt`.

A new login is checked with YouTube before it replaces the saved one: a rejected set is never kept, and without a connection an existing login is left alone. `cmusic logout` deletes the saved login (the desktop app has the same as a core command, `Command::Logout`). Pasted values over 16 kB, or a single cookie over 4 kB, are refused.

Cookies are stored in one owner-only file in the config folder.

## Build
```
cargo build --release
```
Run the release build: without a GPU driver (WSL, virtual machines) the desktop app renders in software, which is only smooth when optimized.

Needs the libmpv development files (`libmpv-dev` on Debian/Ubuntu, `brew install mpv` on macOS).

## Keyboard (desktop app)
Keys other than the Ctrl ones do nothing while a text field has focus.

| Key | Action |
|---|---|
| `Space` | Play / pause |
| `/` or `Ctrl+F` | Focus the search field (`Ctrl+F` also works inside a field) |
| `Esc` | Close login or lyrics, otherwise go Back |
| `Left` / `Right` | Seek -5 s / +5 s |
| `Ctrl+Left` / `Ctrl+Right` | Previous / next track |
| `Up` / `Down` | Volume +5 / -5 |
| `Ctrl+L` | Show / hide lyrics (clicking the cover or title does the same) |
| `Ctrl+T` | Next theme |
| `Ctrl+R` | Re-read the current theme file |

Below 980 px of window width the queue column is hidden; a button in the bar swaps it in for the main area.

## Themes
The desktop app reads user themes from `themes/<name>.json` in the config folder (`~/.config/cmusic/themes/` on Linux). The bundled presets are written there on first run (only if the folder does not exist yet), so copy one to start. A file may hold only the keys it changes; unknown keys or bad values are reported in the status line and the previous look stays.

Press the `Theme: <name>` button next to "Lyrics" to cycle (Default, then each file by name), and `Ctrl+R` to re-read the current file after editing. The choice is remembered. Numbers are clamped to the range shown.

Colours are `"#rrggbb"` or `"#rrggbbaa"`.

| Key | Type | Meaning |
|---|---|---|
| `accent_a`, `accent_b` | colour | Accent gradient start and end (equal = flat accent) |
| `ink` | colour | Window background and text on accent fills |
| `text`, `text_dim` | colour | Main text; times and dimmed lyrics |
| `panel_top`, `panel_mid`, `panel_bottom` | colour | Panel fill at the top edge, at 12 % height, at the bottom (equal = flat panel) |
| `panel_border` | colour | Panel outline and scrollbar thumb |
| `control`, `control_border` | colour | Idle buttons and search field fill and outline (alpha is the base for hover) |
| `scrim_top`, `scrim_bottom` | colour | Dimming layer over the backdrop |
| `base` | 3 colours | Gradient behind the backdrop, or alone when it is off |
| `radius_panel` | number 0-32 | Corner radius of panels |
| `radius_row` | number 0-24 | Corner radius of list rows |
| `gap`, `pad` | number 0-16 | Space between and inside panels; space between items |
| `font_scale` | number 0.8-1.25 | Multiplier for every text size |
| `backdrop` | true/false | Blurred album art behind the UI (false is cheapest to render) |
| `backdrop_blur` | number 0-30 | Blur radius of the backdrop |
| `backdrop_saturation` | number 0-4 | Colour boost of the backdrop |

## Debugging
Environment variables (all front ends):

| Variable | Effect |
|---|---|
| `CMUSIC_LOG=<file>` | Append a log (`<secs> <LEVEL> <target>: <message>`), debug for cmusic and warn for the rest, mpv warnings included. Never holds cookies or stream URLs. `CMUSIC_LOG_LEVEL=trace\|debug\|info\|warn` changes the cmusic level |
| `CMUSIC_AO=<driver>` | mpv audio output, e.g. `null` without audio or `pulse` |
| `CMUSIC_WAYLAND=1` | GUI under WSL: keep Wayland instead of falling back to X11 |
| `CMUSIC_STATE_DIR=<dir>` | Keep session and prefs there, leaving your saved session untouched |

Headless script through the real engine (prints every update, `TTFA <ms> <id>` for time to first audio, `HANG` and exit 3 after 20 s of silence while playing; exit 0 at the end):

```
CMUSIC_AO=null cargo run --release -p cmusic-core --example play -- search:"daft punk" wait:15 seek:60 pause wait:2 resume next wait:10 quit
```

Steps: `play:<id>[,<id>...]`, `search:<query>`, `warm:<id>[,<id>...]` (pre-resolve), `wait:<secs>`, `ttfa` (wait for the pending TTFA line), `seek:<secs>`, `pause`, `resume`, `toggle`, `next`, `prev`, `volume:<0-100>`, `remove:<index>`, `shuffle`, `repeat`, `logout`, `fetch:<liked|playlists|songs|albums|artists>`, `quit`. The old `play -- <id> [secs]` form still works.

Desktop app script: `CMUSIC_DEMO_SCRIPT="step;step;..."` runs the steps in order and is inert when unset. Under WSL the GUI uses X11 even when `WAYLAND_DISPLAY` is set (WSLg's Wayland compositor can stop answering); `CMUSIC_WAYLAND=1` keeps Wayland.

Steps: `search:<query>`, `waitpage` (next page update, max 30 s), `play:<row>`, `wait:<secs>`, `seek:<secs>` and `seekfrac:<0..1>` (a slider drag), `pause`, `next`, `prev`, `lyrics`, `login`, `theme`, `open:<liked|playlists|songs|albums|artists>`, `state` (one line on stderr), `shot:<path.png>`, `quit`.

## License
GPL-3.0-only
