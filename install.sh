#!/usr/bin/env bash
# Builds ViCTR in release mode and atomically installs it to ~/.local/bin/victr
# along with its XDG desktop entry (~/.local/share/applications/victr.desktop)
# and 256x256 icon (~/.local/share/icons/hicolor/256x256/apps/victr.png).
#
# Usage:
#   ./install.sh              # build release + install binary & desktop entry
#   ./install.sh --no-build   # install already-built target/release/victr
#   ./install.sh --hook       # also install git post-merge hook for auto-updates

set -euo pipefail

APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
DEST="$BIN_DIR/victr"
DO_BUILD=1
DO_HOOK=0

for arg in "$@"; do
    case "$arg" in
        --no-build) DO_BUILD=0 ;;
        --hook) DO_HOOK=1 ;;
        -h|--help)
            sed -n '2,9p' "$0" | sed 's/^# \?//'
            exit 0
            ;;
        *)
            echo "Unknown option: $arg" >&2
            exit 1
            ;;
    esac
done

if [[ "$DO_BUILD" -eq 1 ]]; then
    echo "Building ViCTR (release)..."
    cargo build --manifest-path "$APP_DIR/Cargo.toml" --release --offline
fi

BIN_SRC="$APP_DIR/target/release/victr"
if [[ ! -x "$BIN_SRC" ]]; then
    echo "Release binary not found at $BIN_SRC — run without --no-build." >&2
    exit 1
fi

install -Dm755 "$BIN_SRC" "$DEST"
echo "Installed binary -> $DEST"

"$DEST" --install-desktop

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "${XDG_DATA_HOME:-$HOME/.local/share}/applications" 2>/dev/null || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t "${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor" 2>/dev/null || true
fi

if [[ "$DO_HOOK" -eq 1 && -d "$APP_DIR/.git/hooks" ]]; then
    HOOK="$APP_DIR/.git/hooks/post-merge"
    cat > "$HOOK" <<EOF
#!/usr/bin/env bash
# Auto-rebuild and reinstall ViCTR to ~/.local/bin/victr after git pull/merge.
exec "$APP_DIR/install.sh"
EOF
    chmod +x "$HOOK"
    echo "Installed git auto-update hook -> $HOOK"
fi
