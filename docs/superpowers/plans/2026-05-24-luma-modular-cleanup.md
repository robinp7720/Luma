# Luma Modular Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Modularize Luma's largest files without changing launcher behavior.

**Architecture:** Keep `main.rs` as the executable entrypoint and `Sources` as search orchestration. Extract leaf responsibilities first: UI row rendering, power/control action execution, password action helpers, and window source/focus helpers.

**Tech Stack:** Rust 2024, GTK 4, gtk4-layer-shell, clap, serde, existing unit tests.

---

### Task 1: Extract UI Result Rendering

**Files:**
- Create: `src/ui/mod.rs`
- Create: `src/ui/results.rs`
- Modify: `src/main.rs`

- [ ] Move `same_result`, `preserved_selection_index`, `rebuild_results`, `finalize_loaded_results`, `pending_deferred_results`, `background_processing_after_update`, `set_background_processing`, `build_row`, `badge_widget`, `row_tooltip_text`, `move_selection`, and `scroll_row_into_view` from `src/main.rs` into `src/ui/results.rs`.
- [ ] Export only the functions used by `main.rs` as `pub(crate)`.
- [ ] Keep profiling hooks in `main.rs` if moving them causes visibility churn; otherwise pass small callbacks or expose the existing thread-local helpers with crate visibility.
- [ ] Run `cargo test selection_defaults_to_top row_tooltip pending_deferred`.
- [ ] Run `cargo test`.

### Task 2: Extract Power and Desktop Control Actions

**Files:**
- Create: `src/actions/mod.rs`
- Create: `src/actions/power.rs`
- Create: `src/actions/desktop_controls.rs`
- Modify: `src/main.rs`

- [ ] Move power confirmation metadata and session command helpers into `src/actions/power.rs`.
- [ ] Move `DesktopControlCommand`, `desktop_control_commands`, and `execute_desktop_control_operation` into `src/actions/desktop_controls.rs`.
- [ ] Re-export `power_confirmation_results`, `power_requires_confirmation`, `execute_power_operation`, `desktop_control_commands`, and `execute_desktop_control_operation` through `src/actions/mod.rs` only as needed.
- [ ] Run `cargo test power_actions desktop_control_operations`.
- [ ] Run `cargo test`.

### Task 3: Extract Password Action Helpers

**Files:**
- Create: `src/actions/password.rs`
- Modify: `src/actions/mod.rs`
- Modify: `src/main.rs`

- [ ] Move password creation draft types and validation helpers into `src/actions/password.rs` when doing so does not tangle GTK UI state.
- [ ] Move pass loading, password operation execution, copy/autotype helpers, inspected password rows, and custom autotype parsing into `src/actions/password.rs`.
- [ ] Keep UI draft-step functions in `main.rs` if extracting them requires a larger UI state refactor.
- [ ] Run `cargo test password inspected x11_session xwayland`.
- [ ] Run `cargo test`.

### Task 4: Extract Window Source and Focus Helpers

**Files:**
- Create: `src/sources/windows.rs`
- Modify: `src/sources.rs`

- [ ] Move `WindowEntry`, `load_windows`, `focus_window`, `focused_window_target`, `window_focus_command`, `parse_hypr_windows_json`, `parse_niri_windows_json`, `window_result_item`, and window prediction-key helpers into `src/sources/windows.rs`.
- [ ] Re-export the public helpers used by `main.rs` from `src/sources.rs` or update imports directly.
- [ ] Run `cargo test parses_hyprland_windows parses_niri_windows builds_native_focus_command builds_focus_command`.
- [ ] Run `cargo test`.

### Task 5: Final Verification

**Files:**
- Modify as needed based on extraction fallout.

- [ ] Run `cargo fmt`.
- [ ] Run `cargo fmt --check`.
- [ ] Run `cargo test`.
- [ ] Run `cargo build --release --bin Luma`.
- [ ] Run `git diff --check`.
- [ ] Inspect `git diff --stat` and `git status --short --branch`.
