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
