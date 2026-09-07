# nextup

A keyboard-driven terminal to-do manager built around a daily planning pass.

Tasks form a tree — any task can have subtasks, arbitrarily deep, and subtasks
are first-class everywhere (in every list and in the day's top 3). Each morning
you run a planning pass: nextup lists your outstanding work in a fixed order, you
pick the day's top 3, and everything you prioritised in the previous session that
is still open is carried over and auto-ranked below them.

Storage is a single SQLite file that nextup creates and migrates on first run —
there is nothing to set up.

## Install

Requires a recent Rust toolchain. SQLite is compiled in (`rusqlite`'s `bundled`
feature); no system SQLite is needed.

```sh
# from a checkout
cargo install --path .
# or just run it
cargo run --release
```

## Data file

By default the database lives in a per-user data directory:

| OS      | Path                                                        |
| ------- | ---------------------------------------------------------- |
| macOS   | `~/Library/Application Support/nextup/nextup.db`           |
| Linux   | `$XDG_DATA_HOME/nextup/nextup.db` (`~/.local/share/...`)   |
| Windows | `%APPDATA%\nextup\nextup.db`                              |

Override it for a run with `--db <path>`, or set `NEXTUP_DB` in the environment
(handy for throwaway experiments):

```sh
nextup --db /tmp/scratch.db
NEXTUP_DB=/tmp/scratch.db nextup
```

`--help` / `--version` print the usual and exit.

## Screens

- **Outstanding** — every open task, in daily order: `in_progress` first, then
  `on_hold`, then the rest, oldest first within each group. Subtasks appear as
  flat rows with an ancestor breadcrumb.
- **Detail** — one task's fields, its ancestor breadcrumb, and its subtree
  (indented, collapsible). Jump straight to the parent from here.
- **Completed** — finished tasks (`done` / `abandoned`), newest completion first.
- **Planning** — build today's plan: the candidate list in the required order,
  each row showing the priority it held in the last session; assign three of
  them to slots 1/2/3 and commit.
- **Today** — the committed plan: slots 1–3 then carried-over 4+, with quick
  status changes and a slot-swap that benches the replaced task to `on_hold`.

## Keybindings

### Everywhere

| Key            | Action                                    |
| -------------- | ----------------------------------------- |
| `j` / `k` / ↓ / ↑ | move the selection                    |
| `g` / `G` / Home / End | jump to first / last            |
| `Enter` / `l`  | open the selected item                    |
| `o` / `c`      | Outstanding / Completed screen            |
| `s` / `t`      | Planning / Today screen                   |
| `Esc`          | back out (quits from Outstanding)         |
| `q` / `Ctrl-C` | quit                                      |

### Lists and Detail

| Key       | Action                                       |
| --------- | -------------------------------------------- |
| `n`       | new top-level task                           |
| `a`       | new subtask of the selected / shown task     |
| `e` / `E` | edit title / notes                           |
| `r`       | set priority (blank clears it)               |
| `space`   | change status (picker)                       |
| `d`       | delete (confirmed; subtasks go too)          |

### Detail only

| Key        | Action                        |
| ---------- | ----------------------------- |
| `p`        | jump to the parent task       |
| `Tab` / `z`| fold / unfold the selected subtree node |

### Planning

| Key           | Action                                              |
| ------------- | -------------------------------------------------- |
| `1` / `2` / `3` | assign the selected candidate to that slot (press again to unassign) |
| `0` / `x`     | clear the selected candidate's slot                |
| `Enter`       | commit (asks first if today was already planned)   |
| `Esc`         | leave without committing                           |

### Today

| Key     | Action                                                     |
| ------- | -------------------------------------------------------- |
| `space` | change the selected item's status                        |
| `w`     | swap a slot 1–3 item — pick a replacement; the old one goes `on_hold` and drops to the end |
| `s`     | re-plan today                                            |

## Domain rules worth knowing

- **Completion cascades.** Marking a task `done` or `abandoned` gives every
  still-open descendant the same status and the *same* completion timestamp.
- **Reopening does not un-cascade.** Moving a task back to an outstanding status
  clears only its own completion time; subtasks that were completed stay
  completed. nextup asks before reopening so this isn't a surprise.
- **Carry-over** = a task that had a numeric priority in the most recent prior
  planning session and is still outstanding. Skipped days (weekends) are handled
  naturally — the "prior session" is just the last one that happened.
- **Auto-rank.** Carried-over tasks are re-ranked by their previous priority:
  last session's #1 becomes today's #4, #2 → #5, and so on.

## Development

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

The crate is split lib/bin: all domain logic (`model`, `store`, `planning`) and
the terminal-independent app state (`app`) are unit- and integration-tested
without a terminal; the TUI is covered end-to-end with `ratatui`'s `TestBackend`.
See `WORKLOG.md` for the phase-by-phase build history.
