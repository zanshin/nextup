# WORKLOG

## 2026-09-07 — Phase 0: Scaffolding

- `cargo init` as a lib/bin crate named `nextup` (edition 2024), git initialised.
- Added dependencies: `ratatui`, `crossterm`, `rusqlite` (`bundled`),
  `rusqlite_migration`, `chrono`, `directories`, `anyhow`, `thiserror`; dev-dep
  `tempfile`.
- Module skeleton under `src/`: `lib.rs` wires `model`, `store`, `planning`,
  `app`, `ui`, `event`. Each module carries its file-level doc comment and a note
  pointing at the phase that fills it in.
- `main.rs`:
  - Hand-rolled arg parsing — `--db <path>` / `--db=<path>`, `-h`/`--help`,
    `-V`/`--version`, unknown args error out.
  - DB path resolution: `--db` > `NEXTUP_DB` env > `ProjectDirs` default
    (`~/Library/Application Support/nextup/nextup.db` on macOS).
  - `install_panic_hook()` restores the terminal via `TerminalGuard::restore()`
    before delegating to the default hook.
  - `TerminalGuard` RAII struct: `new()` / `Drop` / shared `restore()`. All
    no-ops until Phase 4 wires in raw mode + alternate screen.
- Verified: `cargo build`, `cargo fmt --check`, `cargo clippy --all-targets --
  -D warnings`, `cargo test` (0 tests) all clean. `--help` / `--version` /
  bad-arg / `--db` runs behave as expected.
