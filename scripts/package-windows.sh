#!/usr/bin/env bash
# Builds a self-contained Windows package: dist/cmusic-<version>-windows-x86_64.zip
#
# Run from Git Bash with the build environment loaded (Rust GNU toolchain, WinLibs gcc, libmpv
# dev files). The zip holds everything cmusic needs at run time, next to the executables:
#   cmusic-gui.exe, cmusic.exe   the app (GUI, terminal UI)
#   libmpv-2.dll                 the player
#   yt-dlp.exe                   stream resolver
#   deno.exe                     JS runtime yt-dlp needs for logged-in streams
#
# CMUSIC_DEPS (default ~/.local/cmusic-deps) must hold mpv/libmpv-2.dll, bin/yt-dlp.exe and
# bin/deno.exe. Pass --no-build to package the binaries already in target/release.
set -euo pipefail

cd "$(dirname "$0")/.."
DEPS="${CMUSIC_DEPS:-$HOME/.local/cmusic-deps}"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
name="cmusic-$version-windows-x86_64"
out="dist/$name"

for f in "$DEPS/mpv/libmpv-2.dll" "$DEPS/bin/yt-dlp.exe" "$DEPS/bin/deno.exe"; do
    [ -f "$f" ] || { echo "missing $f" >&2; exit 1; }
done

if [ "${1:-}" != "--no-build" ]; then
    cargo build --release -p cmusic-gui -p cmusic
fi

rm -rf "$out" "dist/$name.zip"
mkdir -p "$out"
cp target/release/cmusic-gui.exe target/release/cmusic.exe "$out/"
cp "$DEPS/mpv/libmpv-2.dll" "$DEPS/bin/yt-dlp.exe" "$DEPS/bin/deno.exe" "$out/"
cp LICENSE "$out/LICENSE.txt"
cat > "$out/README.txt" <<'EOF'
cmusic for Windows

Start cmusic-gui.exe (or cmusic.exe in a terminal for the text UI). Keep all files in this
folder together: the app uses the player (libmpv-2.dll), yt-dlp.exe and deno.exe from here.

Log in: Log in in the app, then paste the Cookie header from music.youtube.com (the app shows
the steps).

If songs stop playing after a YouTube change, update yt-dlp: run "yt-dlp.exe -U" in this folder.

Settings and themes: %APPDATA%\cmusic\config
EOF

# Every DLL the executables import must be a Windows one or shipped here.
if command -v objdump >/dev/null; then
    for exe in "$out"/*.exe "$out"/*.dll; do
        objdump -p "$exe" 2>/dev/null | sed -n 's/^\s*DLL Name: //p'
    done | sort -fu | while read -r dll; do
        lower="$(echo "$dll" | tr 'A-Z' 'a-z')"
        if [ ! -f "$out/$dll" ] && [ ! -f "/c/Windows/System32/$lower" ] \
            && [ ! -f "/c/Windows/System32/$dll" ] && [ "$lower" != "vulkan-1.dll" ] \
            && [[ "$lower" != api-ms-win-* ]]; then
            echo "warning: $dll is neither bundled nor in System32" >&2
        fi
    done
fi

powershell.exe -NoProfile -Command \
    "Compress-Archive -Path '$(cygpath -w "$out")' -DestinationPath '$(cygpath -w "dist/$name.zip")'"
echo "dist/$name.zip ($(du -h "dist/$name.zip" | cut -f1))"
