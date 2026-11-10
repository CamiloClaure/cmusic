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

Cookies are stored in one owner-only file in the config folder.

## Build
```
cargo build --release
```
Run the release build: without a GPU driver (WSL, virtual machines) the desktop app renders in software, which is only smooth when optimized.

Needs the libmpv development files (`libmpv-dev` on Debian/Ubuntu, `brew install mpv` on macOS).

## License
GPL-3.0-only
