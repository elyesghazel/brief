# brief

**A keyboard-first mail client for Hyprland.** Rust and GTK4, no Electron, no web stack.

Several IMAP/SMTP accounts from different servers in one window, a unified inbox, vim-style
keys, a command palette, fourteen live-switchable themes and HTML mail rendered with tracking
blocked. It opens instantly because everything is cached locally.

```
┌ brief ── All inboxes ──── / search ───────────────────── Refresh ── New message ┐
│ 📨 All inboxes  24 │ Post CH AG            08:37 │                                │
│                    │ Heute da: Paket mit …       │         📨                     │
│ ● Work             │ ─────────────────────────── │   Nothing selected             │
│ 📥 Inbox        12 │ Stadtwerke            07:48 │   j / k to move, Enter to read │
│ 📝 Drafts          │ Herbstliche Highlights …    │                                │
│ 📤 Sent            │ ─────────────────────────── │                                │
│ ● Personal         │ IKEA                    Wed │                                │
│ 📥 Inbox        12 │ Hej! DIRIGERA is …          │                                │
├────────────────────┴─────────────────────────────┴────────────────────────────────┤
│ NORMAL  j k move  Enter read  c write  Ctrl+K commands  ? keys                    │
└───────────────────────────────────────────────────────────────────────────────────┘
```

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
- **Works offline.** The newest bodies are prefetched, reading needs no connection.
- **Fits a tiling desktop.** No client-side decorations by default, one window, a second
  `brief` call focuses the running one.

## Install (Arch)

```sh
pacman -S --needed rust gtk4 webkitgtk-6.0
git clone https://github.com/elyesghazel/brief && cd brief
./install.sh   # builds, installs ~/.local/bin/brief and a desktop entry
```

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

## Accounts

Press `A`, type the address and the password. The servers fill in for Gmail, Outlook, iCloud,
Yahoo, Fastmail, GMX, Bluewin, Proton Bridge and Zoho, and fall back to `mail.<domain>`.

- `Ctrl+T` tests the IMAP and SMTP login and lists what happened, without saving anything.
- `Ctrl+Enter` tests again and adds the account.

The password goes into your keyring via `secret-tool` (libsecret), or into
`~/.config/brief/secrets.toml` with mode 0600 when there is none. Gmail, iCloud, Outlook and
Yahoo need an app password.

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

## Keys

`?` shows all of them in the app, `Ctrl+K` lists every command.

| Keys | |
|---|---|
| `j` `k` / `J` `K` | next / previous in the pane / message from anywhere |
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
| `A` | add account |

Composer: `Ctrl+Enter` send, `Ctrl+E` edit in `$EDITOR`, `Ctrl+O` attach, `Ctrl+S` save draft,
`Tab` takes the address suggestion, `Esc` twice discards.

## Themes

`graphite` (default), `catppuccin-mocha`, `catppuccin-latte`, `rose-pine`, `rose-pine-dawn`,
`gruvbox-dark`, `tokyo-night`, `nord`, `everforest-dark`, `dracula`, `kanagawa`, `one-dark`,
and `eg` / `eg-light`: plain black and white with JetBrains Mono for the chrome, after the
[elyesghazel.ch](https://elyesghazel.ch) design kit.
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

## CLI

```
brief            open (or focus) the window
brief sync       sync every account once without a window, print the folders
brief themes     list themes
brief cat x.eml  print a message the way the reader shows it
```

## Not yet

OAuth2 (Gmail and Microsoft work with app passwords), threading, `mailto:` links, removing an
account from the UI (delete its block in the config).

On NVIDIA brief sets `WEBKIT_DISABLE_DMABUF_RENDERER=1` for itself, the usual fix for blank
WebKit views there.

## License

MIT
