# Wallpaper integrations

Open `wall:` or run `Luma --mode wallpapers`. Luma searches configured directories
for JPG, JPEG, PNG and WebP files, caches thumbnails, and lets you select a display
and whether to update desktop colors. The initial display is the focused output
reported by the adapter. An explicit choice stays selected while the picker is open;
if it disconnects, Luma selects the focused available output (or the first output).
Each result carries its display ID so a later focus change cannot retarget it.

Luma does not require a particular compositor, wallpaper daemon, theme generator,
service manager, or dotfiles directory. No backend is enabled by default.

## Configuration

Add `wallpapers` inside `integrations` in `~/.config/Luma/config.json`
(or `$XDG_CONFIG_HOME/Luma/config.json`), then reopen Luma after exiting its warm
process. The following is an example; supply executable adapters of your own:

```json
{
  "integrations": {
    "wallpapers": {
      "directories": ["~/Pictures/Wallpapers"],
      "recursive": true,
      "backend": {"command": ["~/.local/bin/wallpaper-adapter"]},
      "theme": {
        "command": ["~/.local/bin/theme-adapter"],
        "update_on_selection": false
      }
    }
  }
}
```

Directories can overlap; files and directory symlink cycles are deduplicated.
An empty theme command disables theme updates and the Regenerate theme action.
The checkbox overrides `update_on_selection` for this picker session.
`~/` is expanded in image directories and the executable path. Remaining command
arguments are literal strings, with no shell or environment-variable expansion.

## Protocol version 1

Each invocation handles one JSON request from stdin and returns one JSON object
on stdout. Reserve stdout for the response; diagnostics belong on stderr. Exit
nonzero on failure. Luma sends absolute image paths and executes argument arrays
directly, without a shell. Helpers must validate their requests and invoke their
own subprocesses without interpolating filenames into shell commands.

### Wallpaper status

Request:

```json
{"version":1,"operation":"status"}
```

Response:

```json
{
  "version": 1,
  "outputs": [
    {
      "id": "DP-1",
      "label": "Main display (DP-1)",
      "focused": true,
      "wallpaper": "/home/me/Pictures/Wallpapers/coast.jpg",
      "wallpaper_state": "observed"
    }
  ]
}
```

IDs must be nonempty and unique. `wallpaper` can be null. Use `observed` only when
the image is known to be currently displayed; use `configured` when reading a
saved choice. Luma labels those matches **Current** and **Configured**, respectively.
A status failure produces an error row instead of offering unverified targets.

### Wallpaper apply

```json
{"version":1,"operation":"apply","output":"DP-1","path":"/absolute/coast.jpg"}
```

The backend owns persistence, applying the image, and hotplug/session restoration.
Validate that the output still exists. Do not report success just because a
background command was spawned; wait for the work you own to complete.

Successful response:

```json
{"version":1,"ok":true}
```

### Optional theme hook

After a successful wallpaper apply, when enabled, the theme executable receives:

```json
{"version":1,"operation":"apply","path":"/absolute/coast.jpg"}
```

The **Regenerate theme** control sends the same executable:

```json
{"version":1,"operation":"regenerate"}
```

The hook chooses how to persist its theme image and regenerate colors. It returns
the same success response as wallpaper apply. It must not depend on Luma knowing
its theme generator or saved-state format.

### Errors and deadlines

Status requests have a three-second deadline. Each apply/theme operation has a
30-second deadline. Luma drains stdout/stderr with an 8 MiB limit per stream and
terminates the helper process group on timeout or I/O failure. Superseded searches
cancel status requests; changing searches does not cancel an in-progress apply.
Apply runs off the GTK thread, and repeated activation is disabled while it runs.

A theme failure is reported as **Wallpaper applied; theme update failed**. Luma
does not undo a successful wallpaper change. A timeout may occur after side
effects: inspect backend state before retrying. Helpers should make repeated
requests safe. Work delegated to external services may outlive a killed helper.

## Dotfiles integration

The Robin dotfiles repository supplies `luma-wallpaper-dotfiles` and configures it
at setup time. Its optional theme command is the same binary with the `theme`
argument. The adapter owns Hyprland/Niri discovery, `awww_wallpapers.conf`, awww,
the wallpaper watcher, and Matugen/greeter publication. Those assumptions are not
part of Luma's protocol. Other desktops can provide independent implementations.
