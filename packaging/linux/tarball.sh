#!/usr/bin/env bash
# Makes the Linux release tarball after `cargo build --release`: the binary, the desktop entry and
# the icons in a bin/ + share/ layout that copies straight into ~/.local or /usr/local.
set -euo pipefail
out=${1:-dist}
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
name=brief-$version-linux-x86_64
dir=$out/$name

rm -rf "$dir" "$dir.tar.gz"
mkdir -p "$dir/bin" "$dir/share/applications" "$dir/share/icons"
install -m755 target/release/brief "$dir/bin/brief"
install -m644 packaging/brief.desktop "$dir/share/applications/brief.desktop"
cp -r assets/icons/hicolor "$dir/share/icons/"
cp LICENSE "$dir/"
cat > "$dir/INSTALL.txt" <<NOTE
brief $version for Linux x86_64

Built on Ubuntu 24.04. Needs glibc 2.39 or newer, GTK 4.14 or newer and WebKitGTK 6.0:
Ubuntu 24.04+, Debian 13+, Fedora 40+, Arch and the like.

  Ubuntu, Debian  sudo apt install libgtk-4-1 libwebkitgtk-6.0-4
  Fedora          sudo dnf install gtk4 webkitgtk6.0
  Arch            sudo pacman -S gtk4 webkitgtk-6.0

Install for your user (no root), from this folder:

  cp -r bin share ~/.local/
  gtk-update-icon-cache -qtf ~/.local/share/icons/hicolor
  update-desktop-database -q ~/.local/share/applications

~/.local/bin has to be on PATH. Optional, to open mailto: links in brief:

  xdg-mime default brief.desktop x-scheme-handler/mailto

Passwords go to the Secret Service (secret-tool / libsecret) if it is there, else to
~/.config/brief/secrets.toml (0600). Uninstall: delete ~/.local/bin/brief,
~/.local/share/applications/brief.desktop and ~/.local/share/icons/hicolor/*/apps/ch.elyesghazel.brief*.
Config and cache stay in ~/.config/brief and ~/.cache/brief.
NOTE
tar -C "$out" -czf "$dir.tar.gz" "$name"
echo "$dir.tar.gz"
