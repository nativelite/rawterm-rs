# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-08-28

### Added
- `Decoder` — pure, incremental bytes→events decoding for raw+VT-input
  terminals: printable/UTF-8 chars, Ctrl+letter controls, arrows,
  Home/End/PgUp/PgDn/Ins/Del, F1–F12 (SS3 and CSI), xterm modifier
  parameters, Alt via ESC prefix, and bracketed paste that survives chunk
  splits anywhere (including inside the `ESC[201~` terminator). Lone-ESC
  ambiguity is resolved explicitly via `flush()`, never guessed; payload
  sizes are bounded.
- `Event` / `KeyEvent` / `Key` / `Mods` types with builder-style modifier
  helpers.
- `Terminal` — enter raw mode saving prior state; `read_event(timeout)`
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

[Unreleased]: https://github.com/nativelite/rawterm-rs/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/nativelite/rawterm-rs/releases/tag/v0.1.0
