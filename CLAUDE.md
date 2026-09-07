# nextup

A keyboard-driven terminal to-do manager in Rust. Tasks form a tree (subtasks are
first-class). The core workflow is a daily planning pass: list outstanding work in
a fixed order, pick the day's top 3, and auto-rank everything carried over from
the previous planning session.

## Build / test / lint

Every change must end on all three of these clean, with pristine output:

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## Layout

Single binary crate with a lib/bin split so domain logic is testable without a
terminal.

```
src/
  main.rs      Terminal setup/teardown, panic hook, arg parsing, event loop
  lib.rs       Module wiring / re-exports
  model.rs     Task, Status, TaskId newtype, status<->text, ordering key
  store.rs     Connection + PRAGMA foreign_keys, migrations, all SQL
  planning.rs  Daily planning domain logic (candidates, commit, swap, carry-over)
  app.rs       App state, Screen enum, update()/dispatch() — no ratatui types
  ui.rs        render(&App, &mut Frame) and per-screen widgets
  event.rs     KeyEvent -> Action mapping
tests/
  store.rs     Migrations, CRUD, subtree CTE, cascade
  planning.rs  Multi-day planning sequences, swap
  tui.rs       TestBackend + scripted KeyEvents against a temp DB
```

## Key decisions

- **Persistence:** SQLite via `rusqlite` (`bundled` feature — SQLite compiled in,
  no user setup). `rusqlite_migration` runs migrations on startup keyed off
  `PRAGMA user_version`. Every connection opens with `PRAGMA foreign_keys = ON`.
- **DB location:** `directories::ProjectDirs` default
  (`~/Library/Application Support/nextup/nextup.db` on macOS). Override with
  `--db <path>` or the `NEXTUP_DB` env var.
- **TUI:** `ratatui` + `crossterm`. `TestBackend` drives end-to-end tests.
- **Task text:** `title` (one line) + `notes` (free text, may be empty).
- **Timestamps:** RFC3339 UTC `TEXT` in the DB; rendered in local time.
- **Arg parsing:** hand-rolled (three flags don't justify `clap`).

## Domain rules

- **Outstanding statuses:** `new`, `in_progress`, `on_hold`, `waiting`.
  **Completed:** `done`, `abandoned`.
- **Cascade:** setting a parent to `done`/`abandoned` sets every still-open
  descendant to the same status with the *same* `completed_at`. Moving a task
  back out of a completed status clears only its own `completed_at` (no
  un-cascade — deliberate).
- **Daily list order:** `in_progress` group, then `on_hold` group, then the rest
  of the outstanding set; `created_at` ascending within each group. Top-level
  items and subtasks are flat rows, each with an ancestor breadcrumb.
- **Carried over:** an item with a numeric priority in the most recent *prior*
  planning session that is still outstanding today.
- **Auto-bump:** carried-over items are re-ranked by their prior priority —
  last session's #1 becomes today's #4, #2 -> #5, and so on.

## Status

Phase 6 complete — the daily planning workflow works end to end. All feature
phases done; Phase 7 is polish + docs.

- `model`: `Status`/`TaskId`/`Task`, `is_outstanding`/`is_completed`.
- `store`: `open` + migrations 1 & 2, task CRUD, `children`/`subtree`/`ancestors`,
  cascade delete, `set_status` (+ `pub(crate) set_status_within`),
  `outstanding_ordered`/`completed_ordered`/`task_row` (`TaskRow`), planning-table
  repository fns. All SQL lives here.
- `planning`: `candidates`, `commit_session`, `swap_pick`, `plan_for`, `PlanError`.
- `app`: `Screen` (Outstanding/Completed/Detail/Planning/Today), `Mode`
  (Normal/Editing/Picking/Confirming), `Action`, `App`, `Overlay` (`Editor` /
  `StatusPicker` / `TaskPicker` / `Confirm`). `App::new(conn, today)`;
  `App::update(action, &mut conn)` is the sole state transition, routed by mode.
  Writes reload every view; domain errors surface via `status_message`. No
  ratatui/crossterm types.
- `event`: `action_for(KeyEvent, &Screen, Mode) -> Option<Action>`.
- `ui`: `render(&App, &mut Frame)` — five screens, fold markers, centred
  editor/picker/confirm popups, one-shot `status_message` footer.
- `main`: DB open → `TerminalGuard` → blocking event loop; `today` =
  `Local::now().date_naive()`.

### Keybindings

Global: `j/k`+arrows move, `g/G`/Home/End jump, `Enter`/`l` open, `Esc` back
(quits from Outstanding), `q` / `Ctrl-C` quit, `o`/`c` Outstanding/Completed,
`s` start planning, `t` today's plan.
Lists & Detail: `n` new task, `a` subtask, `e`/`E` edit title/notes, `r`
priority, `space` status picker. Detail: `p` jump to parent, `Tab`/`z` fold.
Planning: `1/2/3` assign selected candidate to a slot (repeat to unassign),
`0`/`x` clear, `Enter` commit (confirms before overwriting an existing plan).
Today: `space` status, `w` swap a top-3 slot (benched task → `on_hold` at 4+).

Covered by 21 unit + 48 integration tests (incl. 17 `TestBackend` e2e). See
WORKLOG.md.
