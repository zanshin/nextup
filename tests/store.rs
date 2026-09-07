//! Integration tests for `nextup::store`: migrating a fresh database, task CRUD,
//! the subtree / ancestors recursive CTEs, and cascade delete via the foreign
//! key. Every test runs against a throwaway file created by `tempfile`.

use nextup::model::{Status, TaskId};
use nextup::store;
use rusqlite::Connection;
use tempfile::TempDir;

/// A fresh migrated database backed by a temp file. The returned `TempDir` must
/// be kept alive for the duration of the test — dropping it deletes the file.
fn fresh_db() -> (TempDir, Connection) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("nextup.db");
    let conn = store::open(&path).expect("open + migrate");

    (dir, conn)
}

#[test]
fn opens_and_migrates_a_fresh_database() {
    let (_dir, conn) = fresh_db();

    // Migration ran: the tasks table exists and is queryable, and foreign keys
    // are enforced on this connection.
    let count: i64 = conn
        .query_row("SELECT count(*) FROM tasks", [], |row| row.get(0))
        .expect("query tasks");
    assert_eq!(count, 0);

    let fk: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .expect("read pragma");
    assert_eq!(fk, 1, "foreign_keys pragma must be ON");
}

#[test]
fn create_and_get_round_trip() {
    let (_dir, conn) = fresh_db();

    let before = chrono::Utc::now();
    let id = store::create_task(&conn, "Buy milk", "2%, from the corner shop").unwrap();
    let after = chrono::Utc::now();

    let task = store::get(&conn, id).unwrap().expect("task exists");
    assert_eq!(task.id, id);
    assert_eq!(task.parent_id, None);
    assert_eq!(task.title, "Buy milk");
    assert_eq!(task.notes, "2%, from the corner shop");
    assert_eq!(task.status, Status::New);
    assert_eq!(task.priority, None);
    assert_eq!(task.completed_at, None);
    assert!(task.created_at >= before && task.created_at <= after);
}

#[test]
fn get_returns_none_for_a_missing_id() {
    let (_dir, conn) = fresh_db();

    assert!(store::get(&conn, TaskId(999)).unwrap().is_none());
}

#[test]
fn update_fields_persists_title_notes_and_priority() {
    let (_dir, conn) = fresh_db();

    let id = store::create_task(&conn, "Draft", "").unwrap();
    store::update_fields(&conn, id, "Draft report", "outline first", Some(2)).unwrap();

    let task = store::get(&conn, id).unwrap().unwrap();
    assert_eq!(task.title, "Draft report");
    assert_eq!(task.notes, "outline first");
    assert_eq!(task.priority, Some(2));
    assert_eq!(
        task.status,
        Status::New,
        "update_fields must not touch status"
    );
}

#[test]
fn create_subtask_sets_the_parent() {
    let (_dir, conn) = fresh_db();

    let parent = store::create_task(&conn, "Ship v1", "").unwrap();
    let child = store::create_subtask(&conn, parent, "Write changelog", "").unwrap();

    let task = store::get(&conn, child).unwrap().unwrap();
    assert_eq!(task.parent_id, Some(parent));
}

#[test]
fn create_subtask_rejects_a_missing_parent() {
    let (_dir, conn) = fresh_db();

    let err = store::create_subtask(&conn, TaskId(404), "orphan", "");
    assert!(err.is_err(), "foreign key must reject a nonexistent parent");
}

#[test]
fn children_are_ordered_oldest_first() {
    let (_dir, conn) = fresh_db();

    let parent = store::create_task(&conn, "Trip", "").unwrap();
    let a = store::create_subtask(&conn, parent, "Book flights", "").unwrap();
    let b = store::create_subtask(&conn, parent, "Book hotel", "").unwrap();
    let c = store::create_subtask(&conn, parent, "Pack", "").unwrap();

    let ids: Vec<TaskId> = store::children(&conn, parent)
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(ids, vec![a, b, c]);
}

#[test]
fn subtree_returns_the_whole_tree_pre_order_with_depth() {
    let (_dir, conn) = fresh_db();

    // root
    //  ├─ front
    //  │   └─ front_deep
    //  └─ back
    let root = store::create_task(&conn, "root", "").unwrap();
    let front = store::create_subtask(&conn, root, "front", "").unwrap();
    let front_deep = store::create_subtask(&conn, front, "front_deep", "").unwrap();
    let back = store::create_subtask(&conn, root, "back", "").unwrap();

    let nodes = store::subtree(&conn, root).unwrap();
    let shape: Vec<(TaskId, usize)> = nodes.iter().map(|n| (n.task.id, n.depth)).collect();

    assert_eq!(
        shape,
        vec![(root, 0), (front, 1), (front_deep, 2), (back, 1),],
    );
}

#[test]
fn ancestors_walk_from_a_deep_subtask_up_to_the_root() {
    let (_dir, conn) = fresh_db();

    let root = store::create_task(&conn, "root", "").unwrap();
    let mid = store::create_subtask(&conn, root, "mid", "").unwrap();
    let leaf = store::create_subtask(&conn, mid, "leaf", "").unwrap();

    let ids: Vec<TaskId> = store::ancestors(&conn, leaf)
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(
        ids,
        vec![root, mid],
        "root-first, excluding the task itself"
    );

    assert!(store::ancestors(&conn, root).unwrap().is_empty());
}

#[test]
fn delete_cascades_to_every_descendant() {
    let (_dir, conn) = fresh_db();

    let root = store::create_task(&conn, "root", "").unwrap();
    let mid = store::create_subtask(&conn, root, "mid", "").unwrap();
    let leaf = store::create_subtask(&conn, mid, "leaf", "").unwrap();
    let sibling = store::create_task(&conn, "unrelated", "").unwrap();

    store::delete(&conn, root).unwrap();

    assert!(store::get(&conn, root).unwrap().is_none());
    assert!(store::get(&conn, mid).unwrap().is_none());
    assert!(store::get(&conn, leaf).unwrap().is_none());
    assert!(
        store::get(&conn, sibling).unwrap().is_some(),
        "unrelated tasks are untouched",
    );
}
