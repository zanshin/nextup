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

/// The full migration set, applied in order and keyed off `PRAGMA user_version`.
fn migrations() -> Migrations<'static> {
    Migrations::new(vec![M::up(MIGRATION_1_TASKS)])
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
