# Luma Modular Cleanup Design

Date: 2026-05-24

## Goal

Modularize, clean up, and simplify the Luma launcher codebase without losing functionality. The refactor should preserve the current CLI, UI behavior, search behavior, prediction keys, configuration paths, password workflows, async spinner behavior, and runtime integration choices.

## Current Shape

The crate is already small in file count but has two oversized responsibility centers:

- `src/sources.rs` owns source loading, source-specific search, result construction, parsing helpers, control snapshots, email helpers, window integration, ranking, and deferred-result finalization.
- `src/main.rs` owns application startup, GTK construction, result rendering, search-controller async flow, activation routing, password creation, action execution, power/session commands, desktop-control commands, clipboard and autotype helpers, mail opening, CSS, and many UI tests.

The existing behavior is covered by a broad unit-test suite, and the cleanup should use those tests as the main safety rail.

## Scope

This cleanup is behavior-preserving by default. Small behavior-adjacent fixes are allowed only when they are already understood, naturally touched by the refactor, and covered by a focused test.

In scope:

- Split `main.rs` into startup, UI/search-control, rendering, action execution, and focused action-domain modules.
- Split `sources.rs` into orchestration plus smaller source modules for cohesive domains.
- Keep existing result models, action variants, prediction keys, and config schema stable unless a local move requires visibility changes.
- Reduce duplicated utility code where the extraction exposes a clear shared helper.
- Clean obviously dead imports or warnings when the change is isolated and verified.

Out of scope:

- New launcher features.
- Redesigning the GTK surface or visible row layout.
- Replacing the search architecture with a new trait/plugin system.
- Changing prediction scoring, result ordering, or default-results policy.
- Changing the public binary name, config locations, or dotfiles launcher wiring.
- Broad performance work beyond preserving the existing deferred-search behavior.

## Approach

Use a conservative module extraction rather than a new abstraction-heavy architecture.

`src/main.rs` should become the app entrypoint:

- Parse CLI arguments.
- Configure GTK backend.
- Load config and sources.
- Start the GTK application.
- Delegate UI construction and action execution to modules.

Action execution should move behind a small runtime boundary:

- `actions/mod.rs` for the top-level `execute_action` router and shared status/result helpers.
- `actions/password.rs` for password loading, creation flow support, clipboard copy, autotype, OTP, and custom autotype execution.
- `actions/power.rs` for confirmation rows and session/power command execution.
- `actions/desktop_controls.rs` for desktop-control command mapping and spawning.
- `actions/mail.rs` for mail URL strategy and helper routing.

UI and async search flow should move out of `main.rs`:

- `ui/mod.rs` for `build_ui` and high-level widget wiring.
- `ui/search_controller.rs` for `SearchController`, async generation handling, deferred result scheduling, clipboard URL refresh integration, and spinner state.
- `ui/results.rs` for list rebuilding, row rendering, selection preservation, badges, tooltips, and scrolling.
- `ui/style.rs` for launcher CSS and layer-shell constants if that meaningfully reduces `main.rs`.

Search source extraction should keep `Sources` as the orchestration owner:

- `sources.rs` keeps `Sources`, `SearchSnapshot`, deferred-plan logic, `search_snapshot`, `search_deferred_results`, `search_with_clipboard_url`, activation recording, top-level default results, and final sorting APIs.
- `sources/windows.rs` handles window loading, parsing, focus commands, focused-window capture, and window result conversion.
- `sources/local.rs` handles browser bookmarks, recent files, path parsing, and file-search line parsing.
- `sources/email.rs` handles Thunderbird, Evolution helper integration, local mail parsing, email row/result helpers, and email status text.
- `sources/controls.rs` handles control snapshots, control result construction, and status parsers.
- `sources/system.rs` handles command discovery, command existence, SSH host loading, and current-time helpers if those helpers do not fit better elsewhere.

The extraction can be staged. A useful first implementation pass is:

1. Move UI result rendering from `main.rs` into `ui/results.rs`.
2. Move power and desktop-control execution from `main.rs` into `actions/power.rs` and `actions/desktop_controls.rs`.
3. Move password action execution and password creation helpers from `main.rs` into `actions/password.rs`.
4. Move window source/focus code from `sources.rs` into `sources/windows.rs`.
5. Move bookmark/recent-file parsing from `sources.rs` into `sources/local.rs`.
6. Move controls source/parsers from `sources.rs` into `sources/controls.rs`.

If a step creates excessive visibility churn, keep that step smaller rather than forcing the target shape in one edit.

## Data Flow

The data flow remains unchanged:

1. GTK entry text changes.
2. `SearchController` parses the current query through `Sources::search_snapshot`.
3. Immediate results render first.
4. Deferred file/email work runs after the idle delay.
5. Deferred rows append and final results are sorted/limited with the existing ranking helpers.
6. Row activation routes through `Action`.
7. Successful activations record predictions through `Sources::record_activation`.

Password creation remains a UI-led flow:

1. Add-password result starts a draft.
2. The entry captures optional username and URL.
3. A generated password is inserted through `pass insert --multiline`.
4. The created credential is shown as the existing password action menu.

## Error Handling

The refactor should preserve current error behavior:

- Action failures render a status row instead of crashing the launcher.
- Deferred search failures stay best-effort and should not block immediate results.
- Missing optional tools produce existing instruction/status rows where applicable.
- Secrets must continue to flow through stdin-backed commands, not argv.
- Password autotype must continue to preserve the previous-focus target and use X11 typing for XWayland/X11 targets.

## Testing

Before implementation starts, keep the current `cargo test` baseline as the reference.

For each extraction:

- Run the narrow relevant tests before and after the move when possible.
- Preserve existing test names unless a move requires a module path update.
- Add a focused test before any behavior-adjacent fix.
- Run `cargo fmt --check` or `cargo fmt`, then `cargo test` after each meaningful group.

Final verification for the cleanup:

- `cargo fmt --check`
- `cargo test`
- `cargo build --release --bin Luma`

Warnings that already exist in the EDS helper path do not block the refactor unless the cleanup intentionally touches that code and makes them worse.

## Acceptance Criteria

- The crate builds and the full test suite passes.
- The `Luma` binary remains the public executable.
- The visible launcher behavior remains unchanged.
- `src/main.rs` and `src/sources.rs` are substantially smaller and have clearer ownership boundaries.
- Existing predictions, password flows, window focusing, local search prefixes, controls, and mail actions remain compatible with current tests and README behavior.
- No unrelated dotfiles or parent-repo changes are included.
