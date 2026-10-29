#!/bin/sh
# Builds brief and installs it for the current user: ~/.local/bin/brief, a desktop entry and the icons.
set -e
cd "$(dirname "$0")"
if ! command -v cargo >/dev/null; then
    echo "needs rust: pacman -S rustup && rustup default stable" >&2
    exit 1
fi
for dep in gtk4 webkitgtk-6.0; do
    if ! pkg-config --exists "$dep"; then
        echo "needs $dep: pkexec pacman -S gtk4 webkitgtk-6.0" >&2
        exit 1
    fi
done
cargo build --release
install -Dm755 target/release/brief "$HOME/.local/bin/brief"
install -Dm644 packaging/brief.desktop "$HOME/.local/share/applications/brief.desktop"
icons="$HOME/.local/share/icons/hicolor"
(cd assets/icons/hicolor && find . -type f) | while read -r f; do
    install -Dm644 "assets/icons/hicolor/$f" "$icons/$f"
done
[ -f "$icons/index.theme" ] || cp /usr/share/icons/hicolor/index.theme "$icons/" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -qtf "$icons" || true
command -v secret-tool >/dev/null || echo "note: no secret-tool (libsecret); passwords go to ~/.config/brief/secrets.toml (0600)"
fc-list | grep -q "Apple Color Emoji" || echo "tip: for Apple emojis install ttf-apple-emoji (AUR); brief then uses them on its own"
echo "installed ~/.local/bin/brief - run it, press A to add an account"
