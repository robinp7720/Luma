# Luma

Luma is a unified command palette for Linux desktops.

It helps you launch apps, switch windows, search files, search email, run commands, manage passwords, open bookmarks, control desktop state, and trigger common desktop actions from one fast overlay.

Luma also includes a built-in settings panel for interactive configuration. Type `settings` in the launcher to open it, or use the `Open settings` result that appears in the default mode.

## Features

- Application launcher with icon-theme support
- Pacman/paru package search with terminal-backed installs
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
- Desktop controls for media, audio, screen brightness, Bluetooth, network settings, power profiles, screenshots, color picking, and Vigil notification history
- Keyboard shortcut search over Hyprland's described bindings (Niri opens its own overlay)
- Clipboard history search through `cliphist`, restoring text or images with `wl-copy`

## Requirements

Luma is a desktop utility for Linux and expects a working GTK 4 environment.

Optional integrations:

- `localsearch` for file search
- `pacman` or `paru` for package search and installs
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
- `cliphist`, fed by `wl-paste --watch cliphist store`, for clipboard history
- Vigil for notification controls and local notification-history search

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

Luma stays ready for 30 seconds after its last window closes so repeated
shortcuts reuse the application and search index. Hidden windows stop polling.
Set `ui.idle_timeout_seconds` in the JSON config to adjust this interval, or `0`
to exit immediately. Restart Luma after changing this setting.
The search index refreshes on opening once it is 30 seconds old, even during
frequent use, so newly installed applications and bookmarks become searchable.

When Vigil is available, Controls uses its live desktop state. Local command
probes remain available as a fallback and run after typing pauses; they do not
delay native settings or application results.

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
Luma --mode packages
Luma --mode windows
Luma --mode files
Luma --mode pass
Luma --mode email
Luma --mode ssh
Luma --mode controls
Luma --mode keys
Luma --mode clipboard
Luma --mode timers
Luma --mode notifications
Luma --mode wallpapers
```

Wallpaper mode (`wall:`) shows thumbnails of `~/.dotfiles/wallpapers`. Picking one writes it
for the focused output and as the theme source in `scripts/awww_wallpapers.conf`, then restarts
`awww-wallpaper.service`, which reapplies wallpapers and regenerates the matugen theme that
Luma, Vigil, Hyprland and kitty follow live. Thumbnails are cached in `~/.cache/Luma/wallpapers`.

Timer mode (`timer:`) starts Vigil timers from a duration and label such as `25m focus`,
`1h30m` or `90s tea`, and lists running ones with pause, resume and cancel. Notification mode
(`notif:`) searches Vigil's history newest first; Enter offers the notification's actions and
Dismiss. Both modes talk to Vigil's control socket, so the bar island and the lock screen show
the same timers.

Vigil can open Luma on the originating monitor with live quick-settings context:

```bash
Luma --context audio --output DP-5 --placement bar
```

`--placement bar` anchors Luma under the bar's status area on the right; `--placement bar-start`
anchors it under the bar's search button on the left. Both keep the bar-attached styling.

Controls mode also switches Hyprland monitor layouts (`hypr/scripts/modes/`), the night light
(`scripts/night_light.sh`) and regenerates the wallpaper theme.

Luma watches `~/.config/Luma/theme.css` and restyles itself when matugen rewrites it, so a
warm launcher follows wallpaper changes. Screenshot controls run
`~/.dotfiles/scripts/screenshot.sh`, and volume and brightness controls show Vigil's OSD.

Context launches initially show the bar's live state and typed actions. Typing broadens back to
the normal unified search while keeping related controls ranked prominently. When Vigil is
not running, Luma keeps its local desktop controls and marks bar-only features unavailable.

Lock and suspend actions share the dotfiles session lifecycle. Lock delegates
to `~/.dotfiles/scripts/session_lock.sh`, while suspend lets Hypridle acquire
the session lock before system sleep. This keeps Luma, compositor shortcuts,
and idle locking on the same Hyprlock configuration and fallback path.

## Search Syntax

The visible All, Apps, Windows, Controls, and Files filters keep your search text
when switching scope. `Ctrl+1` through `Ctrl+5` select them from the keyboard;
`all:` also returns a dedicated launch mode to unified search. Arrow keys select
results, Enter opens, and Escape closes. `Luma --toggle` closes an already visible
launcher for the same requested mode, while a different mode replaces it.

Search providers run off the UI thread and desktop status commands time out when
a service does not answer. Open windows join the initial ranked results. The
Controls filter includes Vigil's live contexts; opening Luma dismisses an open
quick-settings panel before presenting the launcher.

Luma understands a few lightweight prefixes:

```text
bookmark: rust docs
recent: report
pkg: firefox
pass: github/work
mail: invoices
ssh: web-server
control: volume
keys: screenshot
clip: token
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
- Additional custom metadata fields, each with Copy and Type actions

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
- Vigil notification DND, clear-all, inbox opening, and searchable local history

## Notes

- Predictive history is stored as plain JSON in `~/.local/state/Luma/predictions.json`.
- File search requires `localsearch` to be installed and indexed.
- Bookmark search reads Firefox `places.sqlite` through `sqlite3` when available and Chromium-family `Bookmarks` JSON directly.
- Recent file search reads local `file://` entries from `recently-used.xbel`.
- Package search prefers `paru -Ss` when paru is installed, falls back to `pacman -Ss`, and opens installs in a terminal with `paru -S` or `sudo pacman -S`.
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

Hyprland window activation and logout use the native Lua IPC API (Hyprland 0.55+ with a Lua configuration).

### User styling

Luma keeps its rounded fallback styling when no user CSS is present. CSS loads
in this order: built-in theme, built-in interaction rules,
`${XDG_CONFIG_HOME:-~/.config}/Luma/theme.css`, then `Luma/style.css` in the same
directory. Use `theme.css` for generated palettes and `style.css` for persistent
user overrides, including geometry and motion. Both files are watched for live
reload; deleting an override returns to the preceding style layer.

### Search reliability and discovery

Ctrl+K or **More…** opens a searchable picker for every mode, including
bookmarks and recent files. Type a mode name and press Enter, or use Down/Tab to
choose a mode. Switching modes preserves the search text. Source-count buttons
above the results narrow the same query to a source; counts describe the loaded
results, not the total number of indexed matches. The footer describes the
selected action.

Search uses a fixed set of workers with one pending request per worker. New
queries replace queued requests and cancel obsolete subprocesses. Deferred
providers publish independently after the typing pause. Results remain selected
by identity as other providers finish, and late sources remain reachable even
when the initial list is full.

File, package, clipboard, compositor, and database search commands have a
three-second deadline; desktop probes retain their shorter deadlines, and the
Evolution helper retains its configurable timeout. Subprocess output is drained
while commands run, with an 8 MiB limit on each output stream. Provider failures
for files, packages, and email appear as actionable status rows. Ctrl+R retries
the current search; Enter on a failure row opens settings. A valid search with
no matches remains distinct from a failed provider.

### Learned history

Settings includes per-source learning switches and **Clear learned history**.
Turning off learning also excludes that source's existing history from ranking
and suggestions; it does not delete those records. Ctrl+Delete forgets the
selected search result's learned history without deleting its underlying file,
password, or application. New activations can teach it again.

History is shared across warm launcher windows. Configuration and history writes
use atomic replacement and owner-only file permissions. The JSON configuration
key history_disabled_sources contains source labels such as "Passwords",
"Commands", or "Web"; an empty list preserves the existing learning behavior.

LUMA_PROFILE=1 logs provider durations alongside existing rendering timings.
