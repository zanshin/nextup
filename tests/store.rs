//! Integration tests for `nextup::store`: migrating a fresh database, task CRUD,
//! the subtree / ancestors recursive CTEs, and cascade delete via the foreign
//! key. Every test runs against a throwaway file created by `tempfile`.

use std::time::Duration;

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

// --- Phase 2: status logic + list queries ---------------------------------

fn status_of(conn: &Connection, id: TaskId) -> Status {
    store::get(conn, id).unwrap().unwrap().status
}

fn completed_at(conn: &Connection, id: TaskId) -> Option<chrono::DateTime<chrono::Utc>> {
    store::get(conn, id).unwrap().unwrap().completed_at
}

#[test]
fn completing_a_parent_cascades_one_shared_timestamp_to_open_descendants() {
    let (_dir, mut conn) = fresh_db();

    let root = store::create_task(&conn, "root", "").unwrap();
    let mid = store::create_subtask(&conn, root, "mid", "").unwrap();
    let leaf = store::create_subtask(&conn, mid, "leaf", "").unwrap();

    // A descendant finished earlier keeps its own, earlier completion time.
    let earlier = store::create_subtask(&conn, root, "earlier", "").unwrap();
    store::set_status(&mut conn, earlier, Status::Done).unwrap();
    let earlier_ts = completed_at(&conn, earlier).unwrap();

    std::thread::sleep(Duration::from_millis(5));
    store::set_status(&mut conn, root, Status::Done).unwrap();

    assert_eq!(status_of(&conn, root), Status::Done);
    assert_eq!(status_of(&conn, mid), Status::Done);
    assert_eq!(status_of(&conn, leaf), Status::Done);

    let root_ts = completed_at(&conn, root);
    assert!(root_ts.is_some());
    assert_eq!(
        completed_at(&conn, mid),
        root_ts,
        "mid shares root's timestamp"
    );
    assert_eq!(
        completed_at(&conn, leaf),
        root_ts,
        "leaf shares root's timestamp"
    );

    assert_eq!(
        completed_at(&conn, earlier),
        Some(earlier_ts),
        "an already-completed descendant is not re-stamped",
    );
    assert_ne!(completed_at(&conn, earlier), root_ts);
}

#[test]
fn abandoning_cascades_only_over_still_open_descendants() {
    let (_dir, mut conn) = fresh_db();

    let root = store::create_task(&conn, "root", "").unwrap();
    let open_child = store::create_subtask(&conn, root, "open", "").unwrap();
    let done_child = store::create_subtask(&conn, root, "done", "").unwrap();
    store::set_status(&mut conn, done_child, Status::Done).unwrap();

    store::set_status(&mut conn, root, Status::Abandoned).unwrap();

    assert_eq!(status_of(&conn, root), Status::Abandoned);
    assert_eq!(status_of(&conn, open_child), Status::Abandoned);
    assert_eq!(
        status_of(&conn, done_child),
        Status::Done,
        "a completed descendant keeps its own status",
    );
    assert_eq!(completed_at(&conn, open_child), completed_at(&conn, root));
}

#[test]
fn reopening_clears_only_that_rows_completion_time() {
    let (_dir, mut conn) = fresh_db();

    let root = store::create_task(&conn, "root", "").unwrap();
    let mid = store::create_subtask(&conn, root, "mid", "").unwrap();
    let leaf = store::create_subtask(&conn, mid, "leaf", "").unwrap();

    store::set_status(&mut conn, root, Status::Done).unwrap();
    store::set_status(&mut conn, root, Status::InProgress).unwrap();

    assert_eq!(status_of(&conn, root), Status::InProgress);
    assert_eq!(completed_at(&conn, root), None);

    // Descendants are deliberately left completed (no un-cascade).
    assert_eq!(status_of(&conn, mid), Status::Done);
    assert!(completed_at(&conn, mid).is_some());
    assert_eq!(status_of(&conn, leaf), Status::Done);
    assert!(completed_at(&conn, leaf).is_some());
}

#[test]
fn set_status_rejects_an_unknown_task() {
    let (_dir, mut conn) = fresh_db();

    assert!(store::set_status(&mut conn, TaskId(123), Status::Done).is_err());
}

#[test]
fn outstanding_ordered_groups_in_progress_then_on_hold_then_the_rest() {
    let (_dir, mut conn) = fresh_db();

    // Created in this order; creation order breaks ties within a group.
    let parent = store::create_task(&conn, "parent", "").unwrap();
    let sub = store::create_subtask(&conn, parent, "sub", "").unwrap();
    let solo = store::create_task(&conn, "solo", "").unwrap();
    let held = store::create_task(&conn, "held", "").unwrap();
    let fresh = store::create_task(&conn, "fresh", "").unwrap();
    let finished = store::create_task(&conn, "finished", "").unwrap();

    store::set_status(&mut conn, sub, Status::InProgress).unwrap();
    store::set_status(&mut conn, solo, Status::InProgress).unwrap();
    store::set_status(&mut conn, held, Status::OnHold).unwrap();
    store::set_status(&mut conn, finished, Status::Done).unwrap();

    let rows = store::outstanding_ordered(&conn).unwrap();
    let ids: Vec<TaskId> = rows.iter().map(|r| r.task.id).collect();

    assert_eq!(
        ids,
        vec![sub, solo, held, parent, fresh],
        "in_progress (oldest first), then on_hold, then the rest (oldest first)",
    );

    let sub_row = rows.iter().find(|r| r.task.id == sub).unwrap();
    assert_eq!(sub_row.breadcrumb, vec!["parent".to_string()]);

    let solo_row = rows.iter().find(|r| r.task.id == solo).unwrap();
    assert!(
        solo_row.breadcrumb.is_empty(),
        "top-level rows have no breadcrumb"
    );
}

#[test]
fn completed_ordered_lists_newest_completion_first() {
    let (_dir, mut conn) = fresh_db();

    let a = store::create_task(&conn, "a", "").unwrap();
    store::set_status(&mut conn, a, Status::Done).unwrap();
    std::thread::sleep(Duration::from_millis(5));

    let b = store::create_task(&conn, "b", "").unwrap();
    store::set_status(&mut conn, b, Status::Abandoned).unwrap();
    std::thread::sleep(Duration::from_millis(5));

    // Newest: a parent completed via cascade, so parent and child share a time.
    let root = store::create_task(&conn, "root", "").unwrap();
    let kid = store::create_subtask(&conn, root, "kid", "").unwrap();
    store::set_status(&mut conn, root, Status::Done).unwrap();

    // An outstanding task must not appear.
    let _open = store::create_task(&conn, "open", "").unwrap();

    let ids: Vec<TaskId> = store::completed_ordered(&conn)
        .unwrap()
        .into_iter()
        .map(|r| r.task.id)
        .collect();

    assert_eq!(
        ids,
        vec![root, kid, b, a],
        "id ascending breaks a shared timestamp"
    );
}
