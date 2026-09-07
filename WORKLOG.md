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

## 2026-09-07 — Phase 1: Model + store foundation

- `model.rs` (TDD, 4 unit tests):
  - `Status` enum (`New`, `InProgress`, `OnHold`, `Waiting`, `Abandoned`,
    `Done`) with `as_str` / `Display` / `FromStr`. Text forms are the exact
    values in the migration-1 CHECK constraint; a round-trip test and a
    schema-string test guard the duplication.
  - `UnknownStatus(String)` typed error (`thiserror`) for bad status text.
  - `TaskId(i64)` newtype: `value()`, `Display`, `Ord`, `Hash`.
  - `Task` struct with `chrono::DateTime<Utc>` timestamps.
- `store.rs` (TDD against `tests/store.rs`, 10 integration tests):
  - `open(&Path)`: creates the parent dir, opens the file, `PRAGMA
    foreign_keys = ON`, runs migrations via `rusqlite_migration`.
  - Migration 1 (`tasks` + three indexes) inlined as a `const`.
  - CRUD: `create_task`, `create_subtask` (FK-checked parent), `get`
    (`Option`), `update_fields` (title/notes/priority only — not status),
    `delete` (FK `ON DELETE CASCADE`).
  - Tree nav: `children` (oldest first, id tie-break); `subtree` — recursive
    `descendants` CTE returning `SubtreeNode { task, depth }` in pre-order via
    a zero-padded id path; `ancestors` — recursive walk up `parent_id`,
    root-first, excluding the task itself.
  - Row mapping is by column name; a bad status/timestamp surfaces as a
    rusqlite `FromSqlConversionFailure`. Timestamps stored RFC3339 UTC micros.
- `main.rs` still doesn't open the store (that lands with the Phase 4 event
  loop); migrate-from-empty is covered by `opens_and_migrates_a_fresh_database`.
- Verified: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test` (14 tests) all clean.

## 2026-09-07 — Phase 2: Status logic + list queries

- `model.rs`: `Status::is_outstanding` / `is_completed` predicates + a unit test
  asserting they partition the six statuses exactly.
- `store::set_status(&mut Connection, id, status)` (TDD, transactional):
  - To a completed status: `completed_at = COALESCE(completed_at, now)` on the
    row, then read the effective timestamp back and hand it to every
    still-outstanding descendant (recursive `descendants` CTE + `WITH ... UPDATE`,
    filtered to `status IN <outstanding>`). Already-completed descendants keep
    their own status and timestamp.
  - To an outstanding status: clear this row's `completed_at` only; no
    un-cascade of descendants.
  - Unknown id -> error (checked via rows-affected).
- `store::outstanding_ordered` -> `Vec<TaskRow>`: `WHERE status IN <outstanding>`,
  `ORDER BY CASE status ... END, created_at ASC, id ASC` (in_progress, then
  on_hold, then the rest; oldest first per group).
- `store::completed_ordered` -> `Vec<TaskRow>`: completed set, `ORDER BY
  completed_at DESC, id ASC` (id ascending puts a cascaded parent before its
  children when they share a timestamp).
- New `TaskRow { task, breadcrumb }`; `with_breadcrumbs` attaches ancestor titles
  (root first) via one `ancestors` query per row.
- `OUTSTANDING_SQL` / `COMPLETED_SQL` status-tuple consts; kept in lock-step with
  `model::Status` (guarded by the model round-trip/schema tests).
- Verified: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test` (5 unit + 16 integration) all clean.
