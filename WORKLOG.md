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

## 2026-09-07 — Phase 3: Daily planning logic

- Migration 2: `planning_sessions` (`plan_date` unique) + `planning_items`
  (composite PK `session_id, task_id`, `carried_over` flag), both FK
  `ON DELETE CASCADE`. Registered in `migrations()`.
- New `store` planning-table repository fns (all SQL stays in `store`):
  `planning_session_on`, `latest_planning_session_before` (strict `<` so a
  skipped weekend still carries from the last real session),
  `create_planning_session`, `clear_planning_items`, `upsert_planning_item`
  (`ON CONFLICT … DO UPDATE`), `planning_items` (priority order),
  `set_task_priority`, and `task_row` (task + breadcrumb).
- `store::set_status` refactored into a thin `&mut Connection` transactional
  wrapper over `pub(crate) set_status_within(&Connection, …)` so planning's
  swap can run it inside its own transaction. Phase 2 behaviour/tests
  unchanged.
- `planning.rs`:
  - Pure helpers (unit-tested): `rank_carried_over` (sort by prior priority,
    assign 4,5,6,…, `task_id` tie-break) and `next_auto_priority` (end of the
    4+ range).
  - `PlanError` (`thiserror`): `UnknownTask`, `NotOutstanding`,
    `AlreadyInTopThree`, `BadSlot`, `NoPlan`, plus transparent `Storage`
    wrapping `anyhow`/`rusqlite` errors.
  - `candidates` — `outstanding_ordered` annotated with each task's
    most-recent-prior-session priority.
  - `commit_session(&mut Connection, NaiveDate, [Option<TaskId>; 3])` —
    transactional: upsert/clear the session, slot-indexed picks (priority ==
    slot), then carry over still-outstanding non-picked prior members at
    4,5,6,…; writes `tasks.priority` for every session member and leaves
    completed drop-outs' priority alone. Rejects a non-outstanding pick.
  - `swap_pick(&mut Connection, NaiveDate, slot, TaskId)` — guards
    (`BadSlot`, `NoPlan`, `NotOutstanding`, `AlreadyInTopThree`); benched
    occupant → `on_hold`, `carried_over`, priority to end of 4+ range; new
    task takes the slot.
  - `plan_for` — load a committed `Plan` (or `None`).
- Integration tests (`tests/planning.rs`, 15): day-1 picks + carry 4/5 by
  prior priority; next-day re-rank with a completed task keeping its
  priority; skipped-day carry; re-plan overwrite; swap benches to `on_hold`
  at end of 4+ range; each guard error; 0-outstanding empty plan; picking an
  `in_progress`/`on_hold` task; candidate annotation; mid-day completion
  keeps history but leaves active views; `plan_for`.
- No terminal code. Verified: `cargo fmt --check`, `cargo clippy
  --all-targets -- -D warnings`, `cargo test` (9 unit + 31 integration) clean.

## 2026-09-07 — Phase 4: TUI shell (read-only)

- `app.rs` (no ratatui/crossterm types): `Screen` (`Outstanding` / `Completed`
  / `Detail(TaskId)`), `Action` enum, `App` with per-screen data + selection,
  `Detail { task, ancestors, subtree, selected }`. `App::update(action, &conn)`
  is the single state-transition entry point; it reloads the target screen's
  data from the store on every navigation so views never drift. `Back` from
  Outstanding quits; from a sub-screen returns to Outstanding.
- `event.rs`: `action_for(KeyEvent, &Screen) -> Option<Action>`. Ctrl-C quits;
  global `q`/Esc/arrows+`jkgG`/`o`/`c`; `Enter`/`l` opens; `p` jumps to parent
  (Detail only). 3 unit tests (Ctrl-C vs plain `c`, context sensitivity).
- `ui.rs`: `render(&App, &mut Frame)` — header line, body, dim help footer.
  Outstanding/Completed as `Table`s (pri/status/task+breadcrumb/date);
  Detail = info block (breadcrumb `in: …`, title `[status] priority`, notes)
  over an indented subtree `Table`. Selected row gets a literal `> ` marker
  (`HighlightSpacing::Always`) so snapshots can assert on it. Empty-state text
  for both lists and for a childless Detail.
- `main.rs`: `store::open` first (DB created + migrated before touching the
  terminal), then `TerminalGuard` (real `enable_raw_mode` + alternate screen,
  restored on Drop and by the panic hook), `Terminal<CrosstermBackend>`, and a
  blocking `read()` event loop filtering key-release events.
- `tests/tui.rs` (7 e2e): a `Tui` harness feeding `KeyCode`s through
  `event::action_for` -> `app::update` -> `ui::render` on a `TestBackend`,
  asserting on trimmed buffer text. Covers daily-order + breadcrumb rendering,
  selection-marker navigation, Detail (breadcrumb/notes/subtree) + jump to
  parent, childless Detail, Completed list + `q` quit, Esc back-out chain,
  empty-database states.
- Verified: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test` (12 unit + 38 integration = 50) all clean. Binary confirmed to
  create/migrate the DB before terminal setup.

## 2026-09-07 — Phase 5: TUI task management (writes)

- `app.rs`:
  - `Mode` (Normal / Editing / Picking) derived from `App.overlay`.
  - `Overlay::Editor(Editor)` — single-line editor with a `char`-boundary
    cursor (insert / backspace / left / right / home / end), an `EditTarget`
    (NewRootTask, NewSubtaskOf, Title, Notes, Priority), and a label.
  - `Overlay::StatusPicker(StatusPicker)` — target task + index into
    `Status::ALL`.
  - `App.update(&mut self, action, &mut Connection)` now routes by mode;
    `status_message` is a one-shot footer note cleared on the next key.
  - Openers (`n`/`a`/`e`/`E`/`r`/space) target the selected list row or, on
    Detail, the shown task. Commit paths call `store::create_task` /
    `create_subtask` / `update_fields` / `set_status`, then `reload` every
    view (Detail reload keeps selection + folds, prunes stale collapsed ids).
  - Priority editor: blank clears, otherwise a whole number ≥ 1; bad input
    keeps the overlay open with a message.
  - Detail folding: `collapsed: HashSet<TaskId>`; `visible_descendants`
    walks the pre-order subtree skipping anything under a collapsed node and
    tags each row with `has_children` / `collapsed`.
- `event.rs`: `action_for(key, &Screen, Mode)`. Editing mode → text actions
  (`Char` → `InsertChar`, Enter → `Confirm`, Esc/Ctrl-C → `Cancel`). Picking
  mode → `j`/`k` move, Enter/space `Confirm`, Esc/`q` `Cancel`. Normal mode:
  `n` new, `a` subtask, `e`/`E` title/notes, `r` priority, space status,
  `Tab`/`z` fold (Detail). `s` left free for Phase 6 planning. 5 unit tests.
- `ui.rs`: fold markers (`[+]`/`[-]`) in the Detail subtree; centred popups
  (`Clear` + bordered `Block`) for the editor (with real `set_cursor_position`)
  and the status picker; footer shows `status_message` when set, else
  screen-specific help (overlay help while a modal is open).
- `main.rs`: `event_loop` takes `&mut Connection`; passes `app.mode()` to
  `action_for`.
- `tests/tui.rs`: `typ` / `key` / `selected_row` / `task_by_title` helpers;
  7 new e2e tests including the full plan scenario (create task → 2 subtasks
  → subtask to in_progress reorders to top → parent done cascades into
  Completed with one shared `completed_at`), plus title-edit breadcrumb
  propagation, editor cancel, priority set/reject/clear, fold/unfold, notes
  edit, and Ctrl-C from an open editor.
- Verified: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test` (17 unit + 45 integration = 62) all clean.

## 2026-09-07 — Phase 6: TUI daily planning flow

- `App::new(conn, today: NaiveDate)` — the local date planning treats as "today"
  is injected (main passes `Local::now().date_naive()`; tests fix it).
- New `Screen::Planning` / `Screen::Today`; transient state in
  `App.planning: Option<PlanningState>` / `App.todays_plan: Option<TodayState>`.
- New `Mode::Confirming` and overlays `Overlay::TaskPicker` (pick one task, with
  a `TaskPickPurpose`) and `Overlay::Confirm` (`prompt` + `PendingAction`).
- `event.rs`: `s` → StartPlanning, `t` → ShowToday (both global). Planning keys
  `1/2/3` assign, `0/x/Backspace` clear, `Enter` commit. Today keys `space`
  status, `w` swap. Confirming mode: `y`/Enter confirm, `n`/Esc cancel.
- `app.rs`:
  - `start_planning` loads `planning::candidates`, pre-fills slots 1/2/3 from any
    existing plan, records `has_existing_plan`.
  - `assign_slot` (toggle off on repeat, move between slots), `clear_assignment`.
  - `confirm_planning` → overwrite `Confirm` overlay if a plan exists, else
    `commit_plan` → `planning::commit_session` → land on Today. Domain errors go
    to `status_message` instead of bubbling.
  - `show_today` loads `planning::plan_for`; `reload` refreshes it on the Today
    screen.
  - `begin_swap` builds the candidate list (outstanding minus current top 3),
    opens the `TaskPicker`; confirm runs `planning::swap_pick` then reloads.
  - `action_target` extended for Planning (selected candidate) and Today
    (selected plan item) so the status picker works there.
- `ui.rs`: `render_planning` (slot summary line + candidate table with
  `[n]`/`[ ]` and `was #n` columns), `render_today` (priority / status / task /
  `carried` tag, empty-state text), `render_task_picker`, `render_confirm`;
  per-screen footers.
- `tests/tui.rs`: date-aware harness (`today()`), `row_with` / `row_first_field`
  helpers, 3 e2e: full pick→carry→swap flow, re-plan overwrite confirmation
  (decline then accept), Today empty-state + quick status change. 6 new event
  unit tests.
- Verified: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test` (21 unit + 48 integration = 69) all clean.
