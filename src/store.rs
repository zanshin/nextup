//! SQLite persistence: opening a connection with `PRAGMA foreign_keys = ON`,
//! running migrations on startup, and every repository function the rest of the
//! app uses. All SQL lives here; callers pass a `&Connection` and work in terms
//! of `model` types.

use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, ParseError, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, Row};
use rusqlite_migration::{M, Migrations};

use crate::model::{Status, Task, TaskId};

/// Migration 1: the task tree. The status CHECK values must match
/// `model::Status::as_str`.
const MIGRATION_1_TASKS: &str = "
CREATE TABLE tasks (
    id           INTEGER PRIMARY KEY,
    parent_id    INTEGER REFERENCES tasks(id) ON DELETE CASCADE,
    title        TEXT    NOT NULL,
    notes        TEXT    NOT NULL DEFAULT '',
    status       TEXT    NOT NULL DEFAULT 'new'
                   CHECK (status IN ('new','in_progress','on_hold','waiting','abandoned','done')),
    priority     INTEGER,
    created_at   TEXT    NOT NULL,
    completed_at TEXT
);
CREATE INDEX idx_tasks_parent  ON tasks(parent_id);
CREATE INDEX idx_tasks_status  ON tasks(status);
CREATE INDEX idx_tasks_created ON tasks(created_at);
";

/// Migration 2: the daily planning history. `planning_sessions` / `planning_items`
/// are the source of truth for what was planned each day; `tasks.priority` is a
/// denormalized copy of the latest session's priority for list sorting.
const MIGRATION_2_PLANNING: &str = "
CREATE TABLE planning_sessions (
    id         INTEGER PRIMARY KEY,
    plan_date  TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL
);
CREATE TABLE planning_items (
    session_id   INTEGER NOT NULL REFERENCES planning_sessions(id) ON DELETE CASCADE,
    task_id      INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    priority     INTEGER NOT NULL,
    carried_over INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (session_id, task_id)
);
CREATE INDEX idx_planning_items_task ON planning_items(task_id);
";

/// The full migration set, applied in order and keyed off `PRAGMA user_version`.
fn migrations() -> Migrations<'static> {
    Migrations::new(vec![M::up(MIGRATION_1_TASKS), M::up(MIGRATION_2_PLANNING)])
}

/// A node of a task subtree: the task plus its depth below the queried root
/// (root itself is depth 0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtreeNode {
    pub task: Task,
    pub depth: usize,
}

/// Open (creating if needed) the database at `path`, turn on foreign keys, and
/// run any pending migrations. Creates the parent directory if it is missing.
pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating data directory {}", parent.display()))?;
    }

    let mut conn =
        Connection::open(path).with_context(|| format!("opening database {}", path.display()))?;

    conn.pragma_update(None, "foreign_keys", true)
        .context("enabling foreign_keys")?;

    migrations()
        .to_latest(&mut conn)
        .context("running migrations")?;

    Ok(conn)
}

/// Insert a top-level task (`status = 'new'`, no parent, no priority).
pub fn create_task(conn: &Connection, title: &str, notes: &str) -> Result<TaskId> {
    insert_task(conn, None, title, notes)
}

/// Insert a subtask of `parent`. Fails if `parent` does not exist (foreign key).
pub fn create_subtask(
    conn: &Connection,
    parent: TaskId,
    title: &str,
    notes: &str,
) -> Result<TaskId> {
    insert_task(conn, Some(parent), title, notes)
}

fn insert_task(
    conn: &Connection,
    parent: Option<TaskId>,
    title: &str,
    notes: &str,
) -> Result<TaskId> {
    let created_at = to_rfc3339(Utc::now());

    conn.execute(
        "INSERT INTO tasks (parent_id, title, notes, status, created_at)
         VALUES (?1, ?2, ?3, 'new', ?4)",
        rusqlite::params![parent.map(TaskId::value), title, notes, created_at],
    )
    .context("inserting task")?;

    Ok(TaskId(conn.last_insert_rowid()))
}

/// Fetch a single task by id, or `None` if there is no such row.
pub fn get(conn: &Connection, id: TaskId) -> Result<Option<Task>> {
    let task = conn
        .query_row(
            &format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1"),
            [id.value()],
            row_to_task,
        )
        .optional()
        .context("fetching task")?;

    Ok(task)
}

/// Overwrite the user-editable non-status fields. Status changes go through
/// `set_status` (Phase 2) so completion timestamps and cascade stay correct.
pub fn update_fields(
    conn: &Connection,
    id: TaskId,
    title: &str,
    notes: &str,
    priority: Option<i64>,
) -> Result<()> {
    conn.execute(
        "UPDATE tasks SET title = ?2, notes = ?3, priority = ?4 WHERE id = ?1",
        rusqlite::params![id.value(), title, notes, priority],
    )
    .context("updating task fields")?;

    Ok(())
}

/// Delete a task. Its descendants go with it via `ON DELETE CASCADE`.
pub fn delete(conn: &Connection, id: TaskId) -> Result<()> {
    conn.execute("DELETE FROM tasks WHERE id = ?1", [id.value()])
        .context("deleting task")?;

    Ok(())
}

/// The outstanding status values as an SQL tuple. Must match the variants
/// `model::Status::is_outstanding` returns true for.
const OUTSTANDING_SQL: &str = "('new','in_progress','on_hold','waiting')";

/// The completed status values as an SQL tuple.
const COMPLETED_SQL: &str = "('abandoned','done')";

/// Change a task's status, applying the completion rules:
///
/// - To a completed status: set `completed_at` to now if it was null (keep an
///   existing value otherwise), then give every still-outstanding descendant the
///   same status and that *same* timestamp.
/// - To an outstanding status: clear this row's `completed_at` only. Descendants
///   are left untouched — reopening does not un-cascade.
pub fn set_status(conn: &mut Connection, id: TaskId, status: Status) -> Result<()> {
    let tx = conn
        .transaction()
        .context("beginning set_status transaction")?;

    set_status_within(&tx, id, status)?;

    tx.commit().context("committing set_status transaction")?;

    Ok(())
}

/// The status-change logic without its own transaction, so it can also run
/// inside a caller's transaction (e.g. planning's swap). `conn` may be a
/// `&Transaction`, which derefs to `&Connection`.
pub(crate) fn set_status_within(conn: &Connection, id: TaskId, status: Status) -> Result<()> {
    if status.is_completed() {
        let now = to_rfc3339(Utc::now());

        let updated = conn
            .execute(
                "UPDATE tasks
                 SET status = ?2, completed_at = COALESCE(completed_at, ?3)
                 WHERE id = ?1",
                rusqlite::params![id.value(), status.as_str(), now],
            )
            .context("completing task")?;

        if updated == 0 {
            anyhow::bail!("no task with id {id}");
        }

        // Read back the effective timestamp so descendants share it exactly.
        let stamp: String = conn
            .query_row(
                "SELECT completed_at FROM tasks WHERE id = ?1",
                [id.value()],
                |row| row.get(0),
            )
            .context("reading completion timestamp")?;

        conn.execute(
            &format!(
                "WITH RECURSIVE descendants(id) AS (
                     SELECT id FROM tasks WHERE parent_id = ?1
                   UNION ALL
                     SELECT t.id FROM tasks t JOIN descendants d ON t.parent_id = d.id
                 )
                 UPDATE tasks
                 SET status = ?2, completed_at = ?3
                 WHERE id IN (SELECT id FROM descendants)
                   AND status IN {OUTSTANDING_SQL}"
            ),
            rusqlite::params![id.value(), status.as_str(), stamp],
        )
        .context("cascading completion to descendants")?;
    } else {
        let updated = conn
            .execute(
                "UPDATE tasks SET status = ?2, completed_at = NULL WHERE id = ?1",
                rusqlite::params![id.value(), status.as_str()],
            )
            .context("reopening task")?;

        if updated == 0 {
            anyhow::bail!("no task with id {id}");
        }
    }

    Ok(())
}

/// Direct children of `parent`, oldest first (`id` breaks timestamp ties).
pub fn children(conn: &Connection, parent: TaskId) -> Result<Vec<Task>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {TASK_COLUMNS} FROM tasks
         WHERE parent_id = ?1
         ORDER BY created_at ASC, id ASC"
    ))?;

    let tasks = stmt
        .query_map([parent.value()], row_to_task)?
        .collect::<rusqlite::Result<Vec<Task>>>()
        .context("reading children")?;

    Ok(tasks)
}

/// The whole subtree rooted at `root`, in pre-order (a parent immediately
/// precedes its children), each node carrying its depth below `root`.
pub fn subtree(conn: &Connection, root: TaskId) -> Result<Vec<SubtreeNode>> {
    let mut stmt = conn.prepare(&format!(
        "WITH RECURSIVE descendants(id, depth, path) AS (
             SELECT id, 0, printf('%020d', id)
             FROM tasks WHERE id = ?1
           UNION ALL
             SELECT t.id, d.depth + 1, d.path || '/' || printf('%020d', t.id)
             FROM tasks t
             JOIN descendants d ON t.parent_id = d.id
         )
         SELECT {TASK_COLUMNS_PREFIXED}, d.depth AS depth
         FROM descendants d
         JOIN tasks t ON t.id = d.id
         ORDER BY d.path"
    ))?;

    let nodes = stmt
        .query_map([root.value()], |row| {
            Ok(SubtreeNode {
                task: row_to_task(row)?,
                depth: row.get::<_, i64>("depth")? as usize,
            })
        })?
        .collect::<rusqlite::Result<Vec<SubtreeNode>>>()
        .context("reading subtree")?;

    Ok(nodes)
}

/// Ancestors of `id`, root first, excluding `id` itself. Empty for a top-level
/// task.
pub fn ancestors(conn: &Connection, id: TaskId) -> Result<Vec<Task>> {
    let mut stmt = conn.prepare(&format!(
        "WITH RECURSIVE up(id, depth) AS (
             SELECT parent_id, 1 FROM tasks WHERE id = ?1 AND parent_id IS NOT NULL
           UNION ALL
             SELECT t.parent_id, u.depth + 1
             FROM tasks t
             JOIN up u ON t.id = u.id
             WHERE t.parent_id IS NOT NULL
         )
         SELECT {TASK_COLUMNS_PREFIXED}
         FROM up
         JOIN tasks t ON t.id = up.id
         ORDER BY up.depth DESC"
    ))?;

    let tasks = stmt
        .query_map([id.value()], row_to_task)?
        .collect::<rusqlite::Result<Vec<Task>>>()
        .context("reading ancestors")?;

    Ok(tasks)
}

/// A task paired with its ancestor breadcrumb (ancestor titles, root first), for
/// the flat lists the UI renders. Empty breadcrumb for a top-level task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    pub task: Task,
    pub breadcrumb: Vec<String>,
}

/// Every outstanding task in daily-planning order: `in_progress`, then `on_hold`,
/// then the rest; oldest first within each group. Feeds the Outstanding screen
/// and the planning candidate list.
pub fn outstanding_ordered(conn: &Connection) -> Result<Vec<TaskRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {TASK_COLUMNS} FROM tasks
         WHERE status IN {OUTSTANDING_SQL}
         ORDER BY CASE status
                    WHEN 'in_progress' THEN 0
                    WHEN 'on_hold' THEN 1
                    ELSE 2
                  END,
                  created_at ASC, id ASC"
    ))?;

    let tasks = stmt
        .query_map([], row_to_task)?
        .collect::<rusqlite::Result<Vec<Task>>>()
        .context("reading outstanding tasks")?;

    with_breadcrumbs(conn, tasks)
}

/// Every completed task, newest completion first (`id` ascending breaks a shared
/// timestamp so a cascaded parent lists before its children).
pub fn completed_ordered(conn: &Connection) -> Result<Vec<TaskRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {TASK_COLUMNS} FROM tasks
         WHERE status IN {COMPLETED_SQL}
         ORDER BY completed_at DESC, id ASC"
    ))?;

    let tasks = stmt
        .query_map([], row_to_task)?
        .collect::<rusqlite::Result<Vec<Task>>>()
        .context("reading completed tasks")?;

    with_breadcrumbs(conn, tasks)
}

/// Attach each task's ancestor breadcrumb. One `ancestors` query per row, which
/// is fine at the scale of a personal to-do list.
fn with_breadcrumbs(conn: &Connection, tasks: Vec<Task>) -> Result<Vec<TaskRow>> {
    tasks
        .into_iter()
        .map(|task| {
            let breadcrumb = ancestors(conn, task.id)?
                .into_iter()
                .map(|ancestor| ancestor.title)
                .collect();

            Ok(TaskRow { task, breadcrumb })
        })
        .collect()
}

/// One task plus its breadcrumb, or `None` if the task does not exist.
pub fn task_row(conn: &Connection, id: TaskId) -> Result<Option<TaskRow>> {
    let Some(task) = get(conn, id)? else {
        return Ok(None);
    };

    let breadcrumb = ancestors(conn, id)?
        .into_iter()
        .map(|ancestor| ancestor.title)
        .collect();

    Ok(Some(TaskRow { task, breadcrumb }))
}

// --- planning tables ------------------------------------------------------

/// A row of `planning_sessions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanningSession {
    pub id: i64,
    pub plan_date: String,
}

/// A row of `planning_items`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanningItem {
    pub task_id: TaskId,
    pub priority: i64,
    pub carried_over: bool,
}

fn row_to_session(row: &Row<'_>) -> rusqlite::Result<PlanningSession> {
    Ok(PlanningSession {
        id: row.get("id")?,
        plan_date: row.get("plan_date")?,
    })
}

/// The planning session for exactly `plan_date` (`'YYYY-MM-DD'`), if one exists.
pub fn planning_session_on(conn: &Connection, plan_date: &str) -> Result<Option<PlanningSession>> {
    conn.query_row(
        "SELECT id, plan_date FROM planning_sessions WHERE plan_date = ?1",
        [plan_date],
        row_to_session,
    )
    .optional()
    .context("loading planning session")
}

/// The most recent planning session strictly before `plan_date`. Strict `<` is
/// what lets a skipped day (weekend) still carry over from the last real session.
pub fn latest_planning_session_before(
    conn: &Connection,
    plan_date: &str,
) -> Result<Option<PlanningSession>> {
    conn.query_row(
        "SELECT id, plan_date FROM planning_sessions
         WHERE plan_date < ?1
         ORDER BY plan_date DESC
         LIMIT 1",
        [plan_date],
        row_to_session,
    )
    .optional()
    .context("loading prior planning session")
}

/// Insert a planning session for `plan_date` and return its id.
pub fn create_planning_session(conn: &Connection, plan_date: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO planning_sessions (plan_date, created_at) VALUES (?1, ?2)",
        rusqlite::params![plan_date, to_rfc3339(Utc::now())],
    )
    .context("creating planning session")?;

    Ok(conn.last_insert_rowid())
}

/// Delete every `planning_items` row for a session (re-planning a day starts
/// from a clean slate).
pub fn clear_planning_items(conn: &Connection, session_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM planning_items WHERE session_id = ?1",
        [session_id],
    )
    .context("clearing planning items")?;

    Ok(())
}

/// Insert or update a single planning item (keyed by `session_id, task_id`).
pub fn upsert_planning_item(
    conn: &Connection,
    session_id: i64,
    task_id: TaskId,
    priority: i64,
    carried_over: bool,
) -> Result<()> {
    conn.execute(
        "INSERT INTO planning_items (session_id, task_id, priority, carried_over)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(session_id, task_id)
         DO UPDATE SET priority = excluded.priority, carried_over = excluded.carried_over",
        rusqlite::params![
            session_id,
            task_id.value(),
            priority,
            i64::from(carried_over)
        ],
    )
    .context("upserting planning item")?;

    Ok(())
}

/// All planning items for a session, ordered by priority (then task id).
pub fn planning_items(conn: &Connection, session_id: i64) -> Result<Vec<PlanningItem>> {
    let mut stmt = conn.prepare(
        "SELECT task_id, priority, carried_over FROM planning_items
         WHERE session_id = ?1
         ORDER BY priority ASC, task_id ASC",
    )?;

    let items = stmt
        .query_map([session_id], |row| {
            Ok(PlanningItem {
                task_id: TaskId(row.get("task_id")?),
                priority: row.get("priority")?,
                carried_over: row.get::<_, i64>("carried_over")? != 0,
            })
        })?
        .collect::<rusqlite::Result<Vec<PlanningItem>>>()
        .context("reading planning items")?;

    Ok(items)
}

/// Set (or clear) the denormalized `tasks.priority` used for list sorting.
pub fn set_task_priority(conn: &Connection, id: TaskId, priority: Option<i64>) -> Result<()> {
    conn.execute(
        "UPDATE tasks SET priority = ?2 WHERE id = ?1",
        rusqlite::params![id.value(), priority],
    )
    .context("setting task priority")?;

    Ok(())
}

// --- row mapping -------------------------------------------------------------

/// Column list for a bare `tasks` query, in any order (mapping is by name).
const TASK_COLUMNS: &str =
    "id, parent_id, title, notes, status, priority, created_at, completed_at";

/// Same columns, qualified with the `t` alias for queries that join `tasks AS t`.
const TASK_COLUMNS_PREFIXED: &str = "t.id, t.parent_id, t.title, t.notes, t.status, \
     t.priority, t.created_at, t.completed_at";

/// Map a row selected with `TASK_COLUMNS` (or the prefixed variant) into a
/// `Task`. A bad status or timestamp becomes a conversion error.
fn row_to_task(row: &Row<'_>) -> rusqlite::Result<Task> {
    let id: i64 = row.get("id")?;

    let status_text: String = row.get("status")?;
    let status: Status = status_text.parse().map_err(conversion_error)?;

    let created_at: String = row.get("created_at")?;
    let created_at = parse_rfc3339(&created_at).map_err(conversion_error)?;

    let completed_at: Option<String> = row.get("completed_at")?;
    let completed_at = completed_at
        .map(|s| parse_rfc3339(&s))
        .transpose()
        .map_err(conversion_error)?;

    Ok(Task {
        id: TaskId(id),
        parent_id: row.get::<_, Option<i64>>("parent_id")?.map(TaskId),
        title: row.get("title")?,
        notes: row.get("notes")?,
        status,
        priority: row.get("priority")?,
        created_at,
        completed_at,
    })
}

/// Wrap a domain parse failure as a rusqlite column-conversion error so it can
/// propagate out of `query_map` / `query_row` closures.
fn conversion_error<E>(err: E) -> rusqlite::Error
where
    E: std::error::Error + Send + Sync + 'static,
{
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
}

// --- timestamp helpers -----------------------------------------------------

fn to_rfc3339(ts: DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn parse_rfc3339(s: &str) -> std::result::Result<DateTime<Utc>, ParseError> {
    DateTime::parse_from_rfc3339(s).map(|dt| dt.with_timezone(&Utc))
}
