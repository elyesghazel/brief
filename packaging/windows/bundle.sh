#!/usr/bin/env bash
# Makes the portable Windows folder from an MSYS2 UCRT64 shell after `cargo build --release`:
# brief.exe, the DLLs it loads, and the GTK data it reads at run time. Flat layout - GLib finds
# share/ and lib/ next to its own DLL.
set -euo pipefail
out=${1:-dist/brief}
prefix=${MINGW_PREFIX:-/ucrt64}
pixbuf=lib/gdk-pixbuf-2.0/2.10.0

rm -rf "$out"
mkdir -p "$out/$pixbuf" "$out/share/glib-2.0/schemas" "$out/share/icons"
cp target/release/brief.exe "$out/"

# image loaders (favicons, SVG icons) and the cache that lists them, with relative paths
cp -r "$prefix/$pixbuf/loaders" "$out/$pixbuf/"
sed "s|[^\"]*/$pixbuf/|$pixbuf/|" "$prefix/$pixbuf/loaders.cache" > "$out/$pixbuf/loaders.cache"

# every DLL from the prefix that brief.exe or a loader pulls in
ntldd -R "$out/brief.exe" "$out/$pixbuf"/loaders/*.dll \
    | sed -n 's|.*=> \(.*\\bin\\[^ ]*\.dll\) .*|\1|p' | sort -u \
    | while read -r dll; do
        f=$(cygpath -u "$dll")
        case $f in "$prefix"/bin/*) cp "$f" "$out/" ;; esac
    done

# GTK's own settings schemas (the file dialog reads them)
cp "$prefix"/share/glib-2.0/schemas/org.gtk.gtk4.*.gschema.xml "$out/share/glib-2.0/schemas/"
glib-compile-schemas "$out/share/glib-2.0/schemas"

# Adwaita for the toolbar icons, without the cursors Windows does not use
cp -r "$prefix/share/icons/Adwaita" "$out/share/icons/"
rm -rf "$out/share/icons/Adwaita/cursors"
mkdir -p "$out/share/icons/hicolor"
cp "$prefix/share/icons/hicolor/index.theme" "$out/share/icons/hicolor/"
# brief's own icon, for the default window icon (set_default_icon_name)
cp -r assets/icons/hicolor/. "$out/share/icons/hicolor/"
for t in Adwaita hicolor; do gtk4-update-icon-cache -q -f "$out/share/icons/$t"; done

cp LICENSE "$out/"
du -sh "$out"
