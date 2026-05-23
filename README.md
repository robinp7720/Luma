# Luma

Luma is a unified command palette for Linux desktops.

It helps you launch apps, switch windows, search files, search email, run commands, manage passwords, open bookmarks, control desktop state, and trigger common desktop actions from one fast overlay.

Luma also includes a built-in settings panel for interactive configuration. Type `settings` in the launcher to open it, or use the `Open settings` result that appears in the default mode.

## Features

- Application launcher with icon-theme support
- Active window switching on Hyprland and Niri
- Predictive ranking based on local activation history
- File search through `localsearch`
- Password-store search and native `pass` actions
- On-the-fly password creation with optional username, email, and URL metadata
- Email search with open, reply, compose, and copy-sender actions
- Thunderbird mail database search, EDS-backed Evolution mail, and local maildir-style message files
- SSH host search from `~/.ssh/config`, `known_hosts`, and `known_hosts.old`
- Command runner with `$PATH` suggestions
- Browser bookmark search from Firefox and Chromium-family profiles
- Recent file search from `~/.local/share/recently-used.xbel`
- Web search through the default browser
- `libqalculate` integration through `qalc`
- Desktop controls for media, audio, screen brightness, Bluetooth, network settings, power profiles, screenshots, color picking, and Dunst notification history

## Requirements

Luma is a desktop utility for Linux and expects a working GTK 4 environment.

Optional integrations:

- `localsearch` for file search
- `pass` for password search and creation
- `pass-otp` for OTP inspection actions
- `wtype` on Wayland or `xdotool` on X11 for password autotype
- `wl-copy` on Wayland or `xclip` on X11 for clipboard actions
- `sqlite3` for Firefox bookmark search and Thunderbird email search
- Thunderbird for opening indexed email messages from the local message database
- Evolution and its EDS/Camel libraries for the mail helper backend
- `hyprctl` for Hyprland window switching
- `niri` for Niri window switching
- `qalc` for calculator queries
- `playerctl` for media controls
- `wpctl` for audio controls
- `brightnessctl` for real backlight brightness controls
- `bluetoothctl` for Bluetooth controller status and power toggling
- `nmcli` and `nm-connection-editor` for network status and settings
- `powerprofilesctl` for power profile status and switching
- `grim`, `slurp`, and `wl-copy` for screenshots
- `hyprpicker` for screen color picking
- `dunstctl` for notification controls and local notification-history search

## Configuration

Luma stores its runtime settings in `~/.config/Luma/config.json`.

The settings panel lets you adjust:

- default launcher mode
- window size and layer-shell behavior
- which built-in sources are enabled
- email backend priority, Thunderbird toggle, Evolution toggle, Evolution helper command and timeout, local mail toggle, and mail roots
- web search URL, SSH terminal, password-store path, password clip timeout, and file-search backend
- which desktop-control sources are visible through the unified Controls source

Some appearance changes apply on the next launch.

## Usage

Run from source:

```bash
cargo run --release --bin Luma --manifest-path tools/launcher/Cargo.toml
```

Or run the installed binary directly:

```bash
Luma
```

Optional dedicated modes:

```bash
Luma --mode commands
Luma --mode windows
Luma --mode files
Luma --mode pass
Luma --mode email
Luma --mode ssh
Luma --mode controls
```

## Search Syntax

Luma understands a few lightweight prefixes:

```text
bookmark: rust docs
recent: report
pass: github/work
mail: invoices
ssh: web-server
control: volume
```

In the default mode, bare text is searched across the unified result set. Password queries also support adding new entries:

- If a `pass:` query names an entry that does not exist yet, Luma offers to create it.
- If the clipboard contains a URL, Luma can prefill the new password entry from the host name and store the full URL automatically.
- New password entries are generated locally and can include optional username/email metadata.

## Password Workflow

Password entries use the standard `pass` format:

- First line: the password
- Additional lines: `key: value` metadata

Recognized username keys are `user`, `username`, and `email`. If none of those are present, the entry basename is used.

Password search shows one row per matching entry. Selecting a password entry opens a focused action menu with:

- Autotype
- Copy password
- Copy username
- Type password
- Type username
- Open or copy URL metadata when present
- Copy or type OTP when `pass-otp` metadata is present
- Custom autotype when an entry defines an autotype template

Choosing Autotype types username, Tab, and password into the previously focused window without submitting the form.

## Email Workflow

Email search looks at Thunderbird's local message database, an EDS-backed Evolution helper, and local maildir-style message files.

Matching email rows expose actions for:

- Open message
- Reply to sender
- Compose to sender
- Copy sender address

Reply and compose use `mailto:` for Thunderbird and the Evolution helper for EDS-backed messages. Open uses Thunderbird message URLs when a local profile database is available and otherwise routes through the Evolution helper.

## Desktop Controls

Controls are available in the default result set and can be filtered with `control:`, `controls:`, or `ctl:`.

Control rows expose local desktop actions for:

- Media play/pause, next, and previous through `playerctl`
- Volume up, volume down, mute, and audio settings through `wpctl` and `pavucontrol`
- Screen brightness up/down when `brightnessctl` reports a real backlight device
- Bluetooth controller power toggling through `bluetoothctl`
- Network settings through NetworkManager tools
- Power profile cycling and direct profile selection through `powerprofilesctl`
- Screenshot area/screen actions through `grim`, `slurp`, and `wl-copy`
- Screen color picking through `hyprpicker`
- Dunst notification pause/resume, close-all, history pop, and searchable local notification history

## Notes

- Predictive history is stored as plain JSON in `~/.local/state/Luma/predictions.json`.
- File search requires `localsearch` to be installed and indexed.
- Bookmark search reads Firefox `places.sqlite` through `sqlite3` when available and Chromium-family `Bookmarks` JSON directly.
- Recent file search reads local `file://` entries from `recently-used.xbel`.
- Email search reads Thunderbird's `global-messages-db.sqlite` when available and can also search the Evolution helper plus local `.eml` / maildir-style message files.
- Evolution is opt-in in settings so Luma does not talk to EDS unless you enable it and point it at a helper command if needed.
- The default helper command is `luma-mail-eds`; override it in settings if your build lives elsewhere.
- Window switching uses `hyprctl clients -j` on Hyprland and `niri msg windows --json` on Niri.
- Desktop controls are best-effort and only show rows for tools available on the current system. Notification-history result rows are searchable but are not stored in prediction history.
- Password search reads entry names from `PASSWORD_STORE_DIR` or `~/.password-store`.
- Autotype uses `wtype` on Wayland and `xdotool` on X11. Copying uses `wl-copy` on Wayland and `xclip` on X11, with secrets passed through stdin.
- URL, OTP, and custom autotype rows appear in the focused action menu when the entry contains matching metadata. OTP actions require `pass-otp`.
- Copied password data expires after `PASSWORD_STORE_CLIP_TIME` seconds, defaulting to 15 seconds.
- Web search defaults to DuckDuckGo. Override it with `DOT_LAUNCHER_SEARCH_URL`.
- SSH sessions launch through `~/.dotfiles/scripts/launch_kitty.sh -e ssh <host>`.
