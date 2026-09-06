# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.1] - 2026-08-30

### Changed
- **Mouse capture is now opt-in; text selection works by default.** `raw()`
  previously cleared `ENABLE_QUICK_EDIT_MODE` and enabled `ENABLE_MOUSE_INPUT`
  unconditionally, which disabled the console's native drag-to-select, so a
  host user could not select/copy text. `raw()` now keeps quick-edit **on** and
  does not capture the mouse; a host opts in with the new **`Terminal::set_mouse(on)`**
  only when it actually consumes clicks. (Unix backend gains the same toggle via
  xterm SGR mouse sequences.)

### Added
- **`Terminal::set_mouse(on: bool)`**: toggle mouse capture at runtime.

## [0.2.0] - 2026-08-29

### Added
- **Mouse input.** Raw mode now captures mouse events and surfaces them as
  **SGR mouse escape sequences** (`ESC[<Cb;Cx;Cy` then `M`/`m`) inline in the
  same byte stream `read_bytes` returns, so a passthrough multiplexer forwards
  clicks exactly like keystrokes.
  - **Windows:** the console input mode gains `ENABLE_MOUSE_INPUT` and
    `ENABLE_EXTENDED_FLAGS` and clears `ENABLE_QUICK_EDIT_MODE` (quick-edit
    would otherwise steal clicks for text selection); the saved-mode restore on
    drop is unchanged. Binary `MOUSE_EVENT` records are translated to SGR by a
    pure `encode_mouse` core: left/middle/right press+release (derived from the
    button-state transition), drag (motion with a button held), wheel up/down,
    and shift/alt/ctrl modifier folding. Plain motion with no button held is
    dropped. Unit-tested directly (a live console cannot be injected in CI).
  - **Unix:** no change; a terminal already delivers SGR mouse as bytes once
    the app enables mouse mode, and the Unix reader passes raw bytes through.
- Note: rawterm always *emits* mouse SGR once raw mode is entered. **Gating**
  (only forwarding mouse to an app that requested `?1000h`/`?1006h`) is the
  consumer's job (e.g. atrium tracks the pane's mouse mode); out of scope here.

## [0.1.0] - 2026-08-28

### Added
- `Decoder`: pure, incremental bytes→events decoding for raw+VT-input
  terminals: printable/UTF-8 chars, Ctrl+letter controls, arrows,
  Home/End/PgUp/PgDn/Ins/Del, F1–F12 (SS3 and CSI), xterm modifier
  parameters, Alt via ESC prefix, and bracketed paste that survives chunk
  splits anywhere (including inside the `ESC[201~` terminator). Lone-ESC
  ambiguity is resolved explicitly via `flush()`, never guessed; payload
  sizes are bounded.
- `Event` / `KeyEvent` / `Key` / `Mods` types with builder-style modifier
  helpers.
- `Terminal`: enter raw mode saving prior state; `read_event(timeout)`
  with resize detection by size polling (no global signal handlers);
  `size()`; full restore in `Drop`, panic included; clean error (nothing
  configured) when stdin/stdout is not a terminal.
- Own-FFI platform edges, no `libc`/`windows` crates: Win32 console API
  (VT input mode, `ReadConsoleW` UTF-16 with cross-read surrogate
  handling, VT processing enabled on stdout) and Unix termios +
  `poll`/`read` + `ioctl(TIOCGWINSZ)` with per-OS struct layouts (Linux
  CI-verified; macOS compile-checked).
- Fixture-table test suite verified one-shot and byte-at-a-time, plus
  targeted tests for lone ESC, partial UTF-8, split paste terminators, and
  interleaved feeds.
- Stdlib-only `dev.py` runner (`check`, `test`, `fmt`, `guard`) and the
  Cargo.toml zero-dependency guard.

Third crate in the nativelite **agent terminal** suite (see
`roadmap/agent-terminal-suite.md` in `nativelite/ops`).

[Unreleased]: https://github.com/nativelite/rawterm-rs/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/nativelite/rawterm-rs/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/nativelite/rawterm-rs/releases/tag/v0.1.0
