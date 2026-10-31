# Conventions for this repository

## Names

- A variable, parameter or binding name must say at a glance what the value is. Long names are fine.
- No single-letter names for values: not in `let`, function or closure parameters (`|entry|`, not `|e|`),
  `for` loops, or patterns (`Some(verdict)`, `Err(error)`).
- No cryptic abbreviations: `foreground` not `fg`, `config` not `cfg`, `error` not `err`,
  `context` not `ctx`, `window_handle` not `hwnd`, `request` not `req`, `message` not `msg`.
- Exceptions: generic type parameters (`T`) and lifetimes; names fixed by an external API or a file/wire
  format (serde fields, Win32/X11 names); egui's `ui: &mut egui::Ui` parameter; `id`.

## Documentation

- User-facing docs (`docs/`, `installer/RELEASE_NOTES.md`) are in Korean, in complete sentences with explicit
  subjects and objects. Define every term before using it.

## Design decisions

- Every feature or behaviour change gets an entry at the end of `docs/design_decisions.md` (in Korean): the owner's
  request quoted word for word, the reason the owner gave (or "(말하지 않음)"), and the details the AI chose itself.
  This lets the owner tell later what they ordered and what the AI added.

## Releases

- The owner is the only user. After finishing a requested change (tests pass, pushed), release it without asking:
  bump the rc number in `installer/RELEASE_VERSION`, add the change to the top section of
  `installer/RELEASE_NOTES.md`, commit "Release <version>" and push; `.github/workflows/release.yml` publishes it.
  Then check that the release run succeeded. Several changes in one session may share one release.
