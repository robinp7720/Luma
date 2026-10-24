# Search and history validation

Validated 2026-10-24 using the release binary in an isolated Niri Wayland
session (1280x800, nested on Xvfb). Temporary config, state, home and provider
fixtures were used; personal history, mail and configuration were not changed.

## Automated checks

- cargo test --locked: 180 launcher tests and 4 mail-helper tests passed.
- cargo build --release --locked --bins: passed.
- cargo fmt --check and git diff --check: passed.
- Parent tests/ui_theme_contract_test.sh and tests/vigil_desktop_contract.sh:
  passed.
- New regressions cover request coalescing, cancellation, deadlines, output
  exceeding pipe capacity, atomic private saves, history deletion/exclusion,
  mode-prefix coverage, late-source visibility through desktop merging, and
  Enter waiting for the current query rather than activating stale results.

## UI checks

- Ctrl+K, type "email", Enter selects Email while preserving "report".
  See modes.png and email-mode.png.
- Source-count buttons expose loaded sources and narrow the existing query.
  See source-filter.png.
- A file-provider fixture slept for 20 seconds while local email returned
  independently. The email batch took 0.03 ms; the file process was stopped at
  about 3009 ms. See independent-providers.png.
- Failed file queries show an actionable error, not a no-match result.
  Ctrl+R retries, and Enter opens settings. See provider-failure.png.
- Rapid query replacement while a file process was sleeping remained responsive.
  Keyboard selection stayed on LibreOffice Writer when file results arrived.
- Applications history learning was switched off and saved; the config contained
  Applications in history_disabled_sources.
- Clear learned history persisted an empty JSON object. See history.png and
  history-cleared.png.
- A harmless "true" command was activated, recorded, and then forgotten with
  Ctrl+Delete without executing it again. Tested on the final release binary
  using XTest keyboard input into the nested compositor.
- Opening the mode popover retains the launcher; selecting a mode returns focus
  to its search entry.

The nested compositor used software rendering and emitted Mesa/GDK presentation
warnings. These checks establish behavior and layout, not native GPU performance.
Existing unused/dead-code warnings in the mail helper remain.
