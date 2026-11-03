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

## Tests

- A test that calls the real Claude API gives each step its own prompts: the life rules, the to-dos and the
  user's messages that step needs, written so the expected answer holds at any hour the test runs and does
  not depend on what an earlier step left behind (e.g. sleep hours in the rules make "give me another task"
  fail at night).

## Releases

- The owner is the only user. After finishing a requested change (tests pass, pushed), release it without asking:
  bump the rc number in `installer/RELEASE_VERSION`, write the change in the section "이번 버전에서 바뀐 것" of
  `installer/RELEASE_NOTES.md`, commit "Release <version>" and push; `.github/workflows/release.yml` publishes it.
  Then check that the release run succeeded. Several changes in one session may share one release.
- The release notes hold only what changed in this version (the owner reads older changes on the older releases):
  for a new version, replace that section's content instead of keeping earlier versions' sections below it.
