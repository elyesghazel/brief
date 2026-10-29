<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo/wordmark-dark.svg">
    <img src="assets/logo/wordmark-light.svg" alt="brief" width="240">
  </picture>
</p>

<p align="center"><b>A keyboard-first mail client for Hyprland.</b> Rust and GTK4, no Electron, no web stack.</p>

Several IMAP/SMTP accounts from different servers in one window, a unified inbox, conversations,
vim-style keys, a command palette, fourteen live-switchable themes and HTML mail rendered with
tracking blocked. It opens instantly because everything is cached locally. Runs on Linux, with a
portable build for Windows.

![brief in the eg theme: unified inbox and an open conversation](docs/screenshots/main.png)

<table>
  <tr>
    <td><img src="docs/screenshots/light.png" alt="eg-light theme with an HTML newsletter"></td>
    <td><img src="docs/screenshots/palette.png" alt="Ctrl+K command palette"></td>
  </tr>
  <tr>
    <td><img src="docs/screenshots/themes.png" alt="theme picker with live preview"></td>
    <td><img src="docs/screenshots/compose.png" alt="composer with a reply"></td>
  </tr>
</table>

<sub>Screenshots use made-up accounts and mail.</sub>

## Why

- **Fast.** Headers and recent bodies live in SQLite, so the window opens with your mail
  already there. An incremental sync of two accounts takes about 0.2 s, and new inbox mail
  arrives by IMAP IDLE push.
- **Light.** A 7 MB binary and about 70 MB of RAM with ~2,400 messages cached. WebKit's helper
  processes only start once you open an HTML mail.
- **Keyboard-first.** Every action has a key, `Ctrl+K` finds the rest. The mouse works too.
- **Private.** HTML renders with scripts off, an ephemeral session and a Content Security Policy
  that blocks every remote image, font and stylesheet. A banner says how many were blocked; `I`
  loads them for one mail, **Always for sender** remembers the domain. Links open in your
  browser, never in the view.
- **Rich text.** Bold, italic, links, lists, quotes, headings, inline images and emoji, all
  with keys, sent as HTML with a plain text alternative. Mails without formatting stay plain
  text.
- **Easy setup.** Type your address: brief recognises the provider, signs Gmail in through the
  browser, finds the servers for everything else and checks them before saving.
- **Conversations.** Replies group into one row, your own answers from Sent included; the
  reader folds the older messages to one line each. `threads = false` turns it off.
- **Works offline.** The newest bodies are prefetched, reading needs no connection.
- **Fits a tiling desktop.** No client-side decorations by default, one window, a second
  `brief` call focuses the running one.

## Install (Linux)

**Prebuilt.** Download `brief-<version>-linux-x86_64.tar.gz` from the
[latest release](https://github.com/elyesghazel/brief/releases/latest) and follow its
`INSTALL.txt`. It needs glibc 2.39+, GTK 4.14+ and WebKitGTK 6.0: Arch, Ubuntu 24.04+, Debian
13+, Fedora 40+. Release builds have Google sign-in built in.

**From source (Arch):**

```sh
pacman -S --needed rust gtk4 webkitgtk-6.0
git clone https://github.com/elyesghazel/brief && cd brief
./install.sh   # builds, installs ~/.local/bin/brief, the icon and a desktop entry
```

`install.sh` also makes brief the handler for `mailto:` links, unless another mail app already
has them; then it prints the command to switch:
`xdg-mime default brief.desktop x-scheme-handler/mailto`.

Optional: `ttf-apple-emoji` (AUR). With it installed, brief shows Apple emoji everywhere,
for itself only, through a private fontconfig that hides the other color emoji fonts.

Hyprland bind:

```lua
-- hyprland.lua
hl.bind(mod .. " + SHIFT + M", hl.dsp.exec_cmd("brief"))
```
```ini
# hyprland.conf
bind = $mod SHIFT, M, exec, brief
```

## Install (Windows)

**Installer.** Download `brief-<version>-setup-x86_64.exe` from the
[latest release](https://github.com/elyesghazel/brief/releases/latest) and run it. It installs
for your user only, no admin needed, into `%LOCALAPPDATA%\Programs\brief` (choose "all users"
in the first dialog for Program Files). It adds a Start menu entry, optionally a desktop shortcut,
and optionally offers brief for `mailto:` links: tick it, then pick brief under Settings > Apps >
Default apps > brief. To update, run the newer installer over the old one. Uninstall from
Settings > Apps; it asks whether to delete your settings and mail cache and keeps them unless
you say yes. The builds are not code-signed, so SmartScreen may warn the first time: **More
info**, then **Run anyway**. Silent install: `brief-<version>-setup-x86_64.exe /VERYSILENT /CURRENTUSER`.

**Portable.** Or download `brief-<version>-windows-x86_64-portable.zip`, unzip it anywhere and
run `brief.exe`. GTK and everything it needs sit in the folder, nothing gets installed.

Either way, config is in `%APPDATA%\brief\config.toml`, the cache in
`%LOCALAPPDATA%\brief\mail.db` (`brief --help` prints both).

What differs from Linux:
- HTML mail renders in Microsoft Edge WebView2, which comes with Windows 10 and 11, with the same
  rules as on Linux: scripts off, remote images blocked until `I`, links open in your browser.
  WebView2 only starts with the first HTML mail. Without the WebView2 Runtime
  (https://developer.microsoft.com/microsoft-edge/webview2/) HTML mail shows as text and `o`
  opens it in your browser. Its data lives in `%LOCALAPPDATA%\brief\webview2`.
- Passwords go to the Windows Credential Manager (`brief:<email>`); `password_cmd` runs through
  `cmd`.
- `Ctrl+E` opens `editor` from the config, else `%EDITOR%` in a console window, else Notepad.
- The window has the normal title bar; new-mail notifications are Windows toasts.
- Starting brief again opens a second window instead of focusing the first.

**Building it yourself.** In an MSYS2 UCRT64 shell:
`pacman -S mingw-w64-ucrt-x86_64-{gtk4,librsvg,adwaita-icon-theme,pkgconf,gcc,rust,ntldd} zip`,
then `cargo build --release && packaging/windows/bundle.sh dist/brief` (it also copies
`WebView2Loader.dll`, which brief loads at run time). For the installer, with
[Inno Setup 6.3+](https://jrsoftware.org/isinfo.php):
`iscc /DBundle=%CD%\dist\brief packaging\windows\brief.iss`.
On Linux, `cargo build --no-default-features` builds without WebKit the same way.

## Accounts

On first start brief greets you with a theme picker, then walks you through your first account.
Later, press `A`. Type your address and press `Enter`: brief names the provider, fills in the
servers (built-in presets, then Thunderbird's autoconfig, then `mail.<domain>`) and asks only for
what it needs. `Enter` moves on, `Esc` goes back, `Ctrl+Shift+A` shows the server settings.
brief checks IMAP and SMTP before it saves anything, and says in plain words what to fix if a
step fails.

Passwords go into your keyring via `secret-tool` (libsecret), or into
`~/.config/brief/secrets.toml` with mode 0600 when there is none. iCloud, Yahoo and Outlook need
an app password; the setup links to the page where you make one.

Or by hand in `~/.config/brief/config.toml` (restart afterwards):

```toml
[[account]]
name = "Work"
email = "me@company.ch"
display_name = "Me"
imap = { host = "mail.company.ch", port = 993, security = "tls" }
smtp = { host = "mail.company.ch", port = 587, security = "starttls" }
password_cmd = "secret-tool lookup service brief account me@company.ch"
signature = "-- \nMe"
color = "#f9e2af"
```

`security` is `tls`, `starttls` or `none`. `password_cmd` can be anything that prints the
password: `pass mail/work`, `rbw get work-mail`, ...

To remove an account: `Ctrl+K` -> **Remove account…**, pick it, confirm with `Enter` or `y`.
brief cuts just that block out of the config, forgets the saved password and drops the cached
mail. The mail on the server stays.

### Sign in with Google

Gmail and Google Workspace addresses sign in through the browser: type the address, press
`Enter`, approve brief in the tab that opens, done. No app password, and brief never sees your
Google password; it keeps a refresh token in your keyring (or `secrets.toml`) and renews access
on its own. If Google signs you out (password change, revoked access), the sidebar says so and
`Ctrl+K` -> **Sign in again** fixes it. Accounts written by hand use `auth = "oauth2"` and
`brief login <name>`.

Outlook, Hotmail, Live and Microsoft 365 use a password: an app password for personal Microsoft
accounts (the setup links to the page), your normal one for Microsoft 365 if your admin allows
IMAP and SMTP AUTH.

**Building brief yourself?** Release builds have brief's Google client built in. Your own build
takes it from `BRIEF_GOOGLE_CLIENT_ID` and `BRIEF_GOOGLE_CLIENT_SECRET` at compile time;
without them, add your own in `config.toml`. Create a Google Cloud project, enable the Gmail
API, set up the consent screen with the scopes `https://mail.google.com/`, `openid`, `email`
and `profile`, create an OAuth client of type *Desktop app*, then:

```toml
[oauth.google]
client_id = "1234-abc.apps.googleusercontent.com"
client_secret = "GOCSPX-..."
```

`[oauth.microsoft]` with your own Entra ID app (public client, redirect `http://localhost`,
`IMAP.AccessAsUser.All`, `SMTP.Send`, `offline_access`) works the same way.

## Settings

`,` (or `Ctrl+,`, or ☰ -> **Settings**) opens the settings: Appearance, Reading, Writing,
Accounts, Sync & notifications, Keyboard and About. Everything applies at once and is saved to
`config.toml` right away. brief changes only the line of that setting and keeps your comments
and order; if a hand-edited file is too unusual to change safely, it says so and leaves it alone.

`j` `k` move, `Tab` (or `1`-`7`) switches section, `Enter` toggles or edits, `h` `l` change a
choice, `/` finds a setting, `Esc` goes back. Accounts: rename (the offline mail moves along),
your name, color, signature, servers with a **Test**, sign in again, remove. About has the
config, cache and log paths and a **Check for updates** that only asks GitHub when you press it.

Warnings and errors also go to `brief.log` in the cache folder (rotated at ~1 MB, never
passwords or tokens), handy when brief runs from a launcher.

## Keys

`?` shows all of them in the app, `Ctrl+K` lists every command.

| Keys | |
|---|---|
| `j` `k` / `J` `K` | next / previous in the pane / row from anywhere |
| `n` `p` | next / previous message in a conversation |
| `h` `l` `Tab` `Enter` | switch pane, read |
| `gg` `G` `Ctrl+d` `Ctrl+u` `Space` | jump, half page, page |
| `0` `1`-`9` `[` `]` | all inboxes, account inbox, previous / next folder |
| `gi` `gs` `gd` `ga` `gt` `gj` | inbox, sent, drafts, archive, trash, spam |
| `/` | search: words, `from:` `to:` `subject:` `is:unread` `is:starred` `has:attachment`; Enter also searches the server |
| `c` `r` `R` `f` | compose, reply, reply all, forward |
| `e` `d` `!` `m` | archive, delete, spam, move |
| `s` `u` `U` | star, read / unread, mark all as read |
| `x` `*` | select, select all |
| `I` | load remote images for this mail |
| `o` `O` `H` `L` `a` | browser, browser with images, HTML / text, links, attachments |
| `Ctrl+=` `Ctrl+-` `Ctrl+0` | zoom the HTML view |
| `Ctrl+R` | fetch new mail on every account |
| `t` | theme picker with live preview |
| `,` `Ctrl+,` | settings |
| `A` | add account |
| mailto: link in a mail, or `L` -> link | write to it in brief's composer |

| `E` | edit a draft, or any message as new |

Composer:

| Keys | |
|---|---|
| `Ctrl+Enter` / `Ctrl+S` / `Esc` twice | send / save draft / discard |
| `Ctrl+B` `Ctrl+I` `Ctrl+U` `Ctrl+Shift+X` | bold, italic, underline, strikethrough |
| `Ctrl+L` | link (`Ctrl+click` opens it) |
| `Ctrl+Shift+8` / `7` / `9` | bulleted list / numbered list / quote |
| `Ctrl+Alt+1` / `2` / `0` | heading / subheading / normal text |
| `Ctrl+\` | clear formatting |
| `Ctrl+Shift+P` | insert image (or paste / drop one) |
| `Ctrl+.` | emoji |
| `Ctrl+Shift+M` / `Ctrl+Shift+F` | rich or plain text / show or hide the formatting bar |
| `Ctrl+O` / `Ctrl+E` | attach / edit in `$EDITOR` |

Typing `- `, `1. `, `> `, `# ` or `## ` at the start of a line turns it into a list, quote or
heading. `Tab` takes the address suggestion.

## Themes

`graphite` (default), `catppuccin-mocha`, `catppuccin-latte`, `rose-pine`, `rose-pine-dawn`,
`gruvbox-dark`, `tokyo-night`, `nord`, `everforest-dark`, `dracula`, `kanagawa`, `one-dark`,
and `eg` / `eg-light`: plain black and white with JetBrains Mono for the chrome, after the
[elyesghazel.ch](https://elyesghazel.ch) design kit. `zunder` follows the theme of the zunder
launcher, if you use it.
Press `t` to switch with a live preview; the choice is saved.

Your own: `~/.config/brief/themes/<name>.css` with `@define-color` lines for
`bg fg muted faint ghost accent green sel hover red yellow`. Extra CSS goes in
`~/.config/brief/style.css`.

## Config

`~/.config/brief/config.toml` is written with comments on first start.

| Key | Default | |
|---|---|---|
| `theme` | `"graphite"` | see above |
| `font`, `font_size` | Inter, 10.5 | UI font |
| `html` | `true` | render HTML mail; `false` shows text |
| `favicons` | `"duckduckgo"` | sender icons via DuckDuckGo (the sender sees nothing), `"direct"` from the sender's site, or `"off"` |
| `sync_minutes` | `5` | full sync interval; the inbox is pushed via IDLE anyway |
| `cache_limit` | `1000` | newest messages kept offline per folder |
| `notify` | `true` | desktop notification for new inbox mail |
| `decorations` | `false` | window title bar |
| `editor` | `$TERMINAL -e $EDITOR` | for `Ctrl+E`, e.g. `"kitty -e nvim"` |
| `compose` | `"rich"` | `"plain"` starts the composer without formatting |
| `toolbar` | `true` | formatting bar in the composer |
| `mark_read_ms` | `700` | an open message counts as read after this long; `-1` = never |
| `threads` | `true` | one row per conversation, your Sent replies included; `Ctrl+K` toggles it |

## CLI

```
brief            open (or focus) the window
brief mailto:…   write to a mailto: link, in the running window if there is one
brief login ACC  browser sign-in for an account with auth = "oauth2"
brief sync       sync every account once without a window, print the folders
brief themes     list themes
brief cat x.eml  print a message the way the reader shows it
```

## Notes

- On NVIDIA brief sets `WEBKIT_DISABLE_DMABUF_RENDERER=1` for itself, the usual fix for blank
  WebKit views there.
- The app icon and wordmark are generated from JetBrains Mono outlines by
  `assets/logo/build.py`.

## License

MIT
