#!/bin/sh
# Starts ViCTR using the optimized release build, rebuilding if source changed.
# Any arguments are passed through, e.g.:  ./victr.sh --mpv video.mkv
cd "$(dirname "$(readlink -f "$0")")" || exit 1

release=target/release/victr
stale=""
if [ -x "$release" ]; then
    stale=$(find src Cargo.toml -newer "$release" 2>/dev/null | head -n 1)
fi
if [ ! -x "$release" ] || [ -n "$stale" ]; then
    if command -v cargo >/dev/null 2>&1; then
        cargo build --release
    elif [ -x "$HOME/.cargo/bin/cargo" ]; then
        "$HOME/.cargo/bin/cargo" build --release
    fi
fi

if [ -x "$release" ]; then
    exec "$release" "$@"
fi
exec target/debug/victr "$@"
