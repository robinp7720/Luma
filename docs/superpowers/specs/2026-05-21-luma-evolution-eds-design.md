# Luma Evolution EDS Integration

## Summary

Replace the current Evolution-local-files approach with a proper Evolution Data Server integration. Luma should not crawl `~/.local/share/evolution` or parse SQLite/maildir files itself for Evolution mail. Instead, it should talk to a small helper process that links against Evolution Data Server mail APIs and returns normalized email results and actions.

The launcher keeps owning the user-facing search surface, ranking, and UI. The helper owns Evolution-specific discovery, searching, and message/action resolution.

## Goals

- Search Evolution mail using the live Evolution mail stack rather than local file scraping.
- Keep Thunderbird support intact as a separate backend.
- Keep the launcher fast by isolating heavy EDS/Camel dependencies in a helper binary.
- Support the same user-facing actions for email rows:
  - open message
  - reply to sender
  - compose to sender
  - copy sender address
- Keep Evolution opt-in in settings.
- Preserve the existing `mail:` and `email:` query surface.

## Non-Goals

- Reimplement Evolution itself inside Luma.
- Add IMAP/server-side mail synchronization logic.
- Add calendar or contacts integration in this phase.
- Remove generic local-mail search for non-Evolution mail sources.

## Recommended Architecture

### Two-process boundary

Add a separate helper binary target for Evolution mail, for example `luma-mail-eds`, and keep the current launcher binary focused on UI and orchestration.

The helper will:

- create an `ESourceRegistry`
- create an `EMailSession`
- inspect available mail stores and folders
- search messages through Camel/Evolution mail APIs
- return normalized result rows over JSON
- execute message actions on demand

Luma will:

- invoke the helper when Evolution mail is enabled
- merge the helper results with Thunderbird and any generic local mail sources
- continue to rank and display results in the existing unified result list
- route user actions back to the helper using opaque message identifiers

### Why a helper process

This keeps the launcher build and runtime surface smaller and avoids hard-linking the GTK launcher to the full mail engine stack. It also gives us a stable boundary for testing and for future Evolution-specific quirks without entangling the launcher core. The helper should live as a second binary target in the same Cargo package so the repo stays simple to build and ship.

## Data Flow

### Search

1. Luma receives a `mail:` or `email:` query.
2. Luma determines which mail backends are enabled and preferred.
3. For Evolution, Luma spawns `luma-mail-eds search ...`.
4. The helper queries EDS/Camel and returns a JSON list of normalized rows.
5. Luma maps those rows into `ResultItem`s and merges them with the other email backends.
6. Luma ranks the rows using the configured backend preference and prediction history.

### Open / reply / compose

1. The user activates an Evolution-backed row.
2. Luma sends the action and the opaque message id back to the helper.
3. The helper resolves the message through EDS/Camel state.
4. The helper performs the action using the Evolution mail stack or the best-supported Evolution entry point for that message.

This keeps Luma from needing to understand Evolution storage details, folder internals, or message selection semantics.

## Helper Contract

The helper should expose a small versioned JSON protocol. Suggested commands:

- `search`
- `open`
- `reply`
- `compose`
- `copy-sender`

Suggested search result fields:

- `backend` - always `evolution`
- `message_id` - opaque stable identifier for the helper
- `subject`
- `sender`
- `sender_email`
- `folder`
- `date_label`
- `snippet`
- `openable`
- `replyable`
- `composable`

The launcher does not need to know how the helper resolves the message id. The helper can use the appropriate EDS/Camel identifiers internally, such as store, folder, and message UID references.

## Configuration Changes

Replace the current Evolution local-root settings with helper-oriented settings.

### Keep

- Evolution enabled/disabled toggle
- backend priority ordering
- mixed backend support with Thunderbird and generic local mail

### Add

- helper command path override
- helper timeout
- optional search limit for Evolution results if needed
- no extra startup behavior beyond on-demand search/action calls

### Remove

- Evolution data-root override
- Evolution maildir root scanning

Those were only needed for the old filesystem-based fallback and should not be part of the proper EDS integration.

## Settings Panel

Add an Evolution section to the config panel with:

- enable/disable Evolution mail
- preferred backend ordering
- helper binary path
- helper timeout

The panel should explain that Evolution mail is now sourced from the live mail engine rather than from local file scanning.

## Error Handling

The helper should fail closed:

- If the helper binary is missing, Evolution rows should be omitted and the launcher should show a status row that explains the backend is unavailable.
- If EDS or Camel returns an error, Luma should show a non-fatal status row and keep the rest of the launcher usable.
- If a helper action fails, the user should see an inline failure message rather than the launcher crashing or silently doing nothing.
- Timeouts should be treated as backend failure, not as a fatal launcher error.

## Testing Strategy

### Helper tests

- Search returns normalized rows for a controlled EDS fixture or test profile.
- Open/reply/compose actions resolve the right message identifiers.
- Helper output remains stable and versioned.

### Launcher tests

- Evolution results merge correctly with the existing email backends.
- Backend preference affects ranking.
- Helper failure surfaces as a status row instead of a crash.
- `mail:` and `email:` queries still route correctly.

### Manual validation

- Smoke test against a real Evolution profile on the desktop.
- Verify that opening a result opens the expected message in Evolution.
- Verify reply and compose use the expected Evolution mail composer behavior.

## Rollout Plan

1. Add the helper crate/binary and the JSON protocol.
2. Wire the launcher to use the helper behind the existing Evolution toggle.
3. Remove the filesystem-based Evolution search path.
4. Update the settings panel and README.
5. Verify on a real Evolution profile.

## Decisions

- The helper will be a second binary target in the same Cargo package, not a separate workspace crate.
- Open, reply, and compose will all stay inside the helper so the launcher only ever sees normalized results and opaque action ids.
- The helper will search all configured Evolution stores by default and only gain account scoping later if we need it.
