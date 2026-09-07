//! Integration tests for `nextup::planning`: multi-day planning sequences,
//! carry-over re-ranking, re-planning a day, the swap interaction, and the
//! domain-rule guards. Each test runs against a throwaway `tempfile` database.

use chrono::NaiveDate;
use nextup::model::{Status, TaskId};
use nextup::planning::{self, PlanError};
use nextup::store;
use rusqlite::Connection;
use tempfile::TempDir;

fn fresh_db() -> (TempDir, Connection) {
    let dir = tempfile::tempdir().expect("temp dir");
    let conn = store::open(&dir.path().join("nextup.db")).expect("open + migrate");

    (dir, conn)
}

fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").expect("valid date")
}

fn task(conn: &Connection, title: &str) -> TaskId {
    store::create_task(conn, title, "").expect("create task")
}

fn status_of(conn: &Connection, id: TaskId) -> Status {
    store::get(conn, id).unwrap().unwrap().status
}

fn priority_of(conn: &Connection, id: TaskId) -> Option<i64> {
    store::get(conn, id).unwrap().unwrap().priority
}

/// `(task, priority, carried_over)` for a plan, in priority order.
fn shape(plan: &planning::Plan) -> Vec<(TaskId, i64, bool)> {
    plan.items
        .iter()
        .map(|item| (item.row.task.id, item.priority, item.carried_over))
        .collect()
}

#[test]
fn picks_take_slots_and_prior_session_tasks_carry_over_by_priority() {
    let (_dir, mut conn) = fresh_db();
    let a = task(&conn, "a");
    let b = task(&conn, "b");
    let c = task(&conn, "c");
    let d = task(&conn, "d");
    let e = task(&conn, "e");

    // A prior session establishes priorities 1/2/3 for a, b, c.
    planning::commit_session(&mut conn, date("2026-09-04"), [Some(a), Some(b), Some(c)]).unwrap();

    // Today: pick d, e, a. b and c were prioritized before, are still open and
    // not picked, so they carry over at 4 / 5 ordered by their prior priority.
    let plan = planning::commit_session(&mut conn, date("2026-09-07"), [Some(d), Some(e), Some(a)])
        .unwrap();

    assert_eq!(
        shape(&plan),
        vec![
            (d, 1, false),
            (e, 2, false),
            (a, 3, false),
            (b, 4, true),
            (c, 5, true),
        ],
    );

    assert_eq!(priority_of(&conn, d), Some(1));
    assert_eq!(priority_of(&conn, e), Some(2));
    assert_eq!(priority_of(&conn, a), Some(3));
    assert_eq!(priority_of(&conn, b), Some(4));
    assert_eq!(priority_of(&conn, c), Some(5));
}

#[test]
fn carry_over_reranks_next_day_and_completed_tasks_keep_their_priority() {
    let (_dir, mut conn) = fresh_db();
    let a = task(&conn, "a");
    let b = task(&conn, "b");
    let c = task(&conn, "c");
    let d = task(&conn, "d");

    // Day 0 prioritizes a, b, d.
    planning::commit_session(&mut conn, date("2026-09-05"), [Some(a), Some(b), Some(d)]).unwrap();
    // Day 1: pick a, b, c; d carries over at 4.
    planning::commit_session(&mut conn, date("2026-09-06"), [Some(a), Some(b), Some(c)]).unwrap();

    // b finishes mid-day.
    store::set_status(&mut conn, b, Status::Done).unwrap();

    // Day 2: pick only c. a (prior #1) and d (prior #4) carry over and are
    // re-ranked to 4 / 5; b is done so it does not carry.
    let plan =
        planning::commit_session(&mut conn, date("2026-09-07"), [Some(c), None, None]).unwrap();

    assert_eq!(
        shape(&plan),
        vec![(c, 1, false), (a, 4, true), (d, 5, true)],
    );

    assert_eq!(priority_of(&conn, c), Some(1));
    assert_eq!(priority_of(&conn, a), Some(4));
    assert_eq!(priority_of(&conn, d), Some(5));
    assert_eq!(
        priority_of(&conn, b),
        Some(2),
        "a task that only dropped out by completing keeps its denormalized priority",
    );
}

#[test]
fn a_skipped_day_still_carries_from_the_last_planned_session() {
    let (_dir, mut conn) = fresh_db();
    let a = task(&conn, "a");
    let b = task(&conn, "b");
    let c = task(&conn, "c");

    // Planned Friday; nothing planned over the weekend.
    planning::commit_session(&mut conn, date("2026-09-04"), [Some(a), Some(b), None]).unwrap();

    // Monday: the most recent prior session is still Friday's.
    let plan =
        planning::commit_session(&mut conn, date("2026-09-07"), [Some(c), None, None]).unwrap();

    assert_eq!(
        shape(&plan),
        vec![(c, 1, false), (a, 4, true), (b, 5, true)],
    );
}

#[test]
fn replanning_the_same_day_replaces_the_previous_items() {
    let (_dir, mut conn) = fresh_db();
    let a = task(&conn, "a");
    let b = task(&conn, "b");
    let c = task(&conn, "c");

    planning::commit_session(&mut conn, date("2026-09-07"), [Some(a), Some(b), Some(c)]).unwrap();
    let plan =
        planning::commit_session(&mut conn, date("2026-09-07"), [Some(c), Some(a), None]).unwrap();

    assert_eq!(shape(&plan), vec![(c, 1, false), (a, 2, false)]);
    assert_eq!(priority_of(&conn, c), Some(1));
    assert_eq!(priority_of(&conn, a), Some(2));

    let session = store::planning_session_on(&conn, "2026-09-07")
        .unwrap()
        .unwrap();
    assert_eq!(
        store::planning_items(&conn, session.id).unwrap().len(),
        2,
        "the first commit's items were cleared",
    );

    // b (priority 2 from the first commit) left the plan without completing;
    // its denormalized priority is intentionally left as-is, since only the new
    // session's members are rewritten.
    assert_eq!(priority_of(&conn, b), Some(2));
}

#[test]
fn swap_pick_benches_the_replaced_task_and_promotes_the_new_one() {
    let (_dir, mut conn) = fresh_db();
    let a = task(&conn, "a");
    let b = task(&conn, "b");
    let c = task(&conn, "c");
    let d = task(&conn, "d");
    let e = task(&conn, "e");

    // Prior session so today has carry-over at 4 / 5.
    planning::commit_session(&mut conn, date("2026-09-06"), [Some(a), Some(d), Some(e)]).unwrap();
    planning::commit_session(&mut conn, date("2026-09-07"), [Some(a), Some(b), Some(c)]).unwrap();
    // Today: a@1 b@2 c@3 d@4(carried) e@5(carried)

    let plan = planning::swap_pick(&mut conn, date("2026-09-07"), 2, d).unwrap();

    assert_eq!(
        status_of(&conn, b),
        Status::OnHold,
        "the benched task is put on hold",
    );
    assert_eq!(
        shape(&plan),
        vec![
            (a, 1, false),
            (d, 2, false),
            (c, 3, false),
            (e, 5, true),
            (b, 6, true),
        ],
    );
    assert_eq!(priority_of(&conn, d), Some(2));
    assert_eq!(priority_of(&conn, b), Some(6));
}

#[test]
fn swap_pick_rejects_a_bad_slot() {
    let (_dir, mut conn) = fresh_db();
    let a = task(&conn, "a");
    let b = task(&conn, "b");
    planning::commit_session(&mut conn, date("2026-09-07"), [Some(a), None, None]).unwrap();

    let err = planning::swap_pick(&mut conn, date("2026-09-07"), 4, b).unwrap_err();
    assert!(matches!(err, PlanError::BadSlot(4)));
}

#[test]
fn swap_pick_rejects_a_completed_task() {
    let (_dir, mut conn) = fresh_db();
    let a = task(&conn, "a");
    let b = task(&conn, "b");
    planning::commit_session(&mut conn, date("2026-09-07"), [Some(a), None, None]).unwrap();
    store::set_status(&mut conn, b, Status::Done).unwrap();

    let err = planning::swap_pick(&mut conn, date("2026-09-07"), 1, b).unwrap_err();
    assert!(matches!(err, PlanError::NotOutstanding(id) if id == b));
}

#[test]
fn swap_pick_rejects_a_task_already_in_the_top_three() {
    let (_dir, mut conn) = fresh_db();
    let a = task(&conn, "a");
    let b = task(&conn, "b");
    planning::commit_session(&mut conn, date("2026-09-07"), [Some(a), Some(b), None]).unwrap();

    let err = planning::swap_pick(&mut conn, date("2026-09-07"), 1, b).unwrap_err();
    assert!(matches!(err, PlanError::AlreadyInTopThree(id) if id == b));
}

#[test]
fn swap_pick_requires_an_existing_session() {
    let (_dir, mut conn) = fresh_db();
    let a = task(&conn, "a");

    let err = planning::swap_pick(&mut conn, date("2026-09-07"), 1, a).unwrap_err();
    assert!(matches!(err, PlanError::NoPlan(_)));
}

#[test]
fn commit_session_rejects_a_completed_pick() {
    let (_dir, mut conn) = fresh_db();
    let a = task(&conn, "a");
    store::set_status(&mut conn, a, Status::Done).unwrap();

    let err =
        planning::commit_session(&mut conn, date("2026-09-07"), [Some(a), None, None]).unwrap_err();
    assert!(matches!(err, PlanError::NotOutstanding(id) if id == a));
}

#[test]
fn planning_with_no_outstanding_tasks_commits_an_empty_plan() {
    let (_dir, mut conn) = fresh_db();

    let plan = planning::commit_session(&mut conn, date("2026-09-07"), [None, None, None]).unwrap();
    assert!(plan.items.is_empty());
}

#[test]
fn an_in_progress_or_on_hold_task_can_be_picked() {
    let (_dir, mut conn) = fresh_db();
    let a = task(&conn, "a");
    let b = task(&conn, "b");
    store::set_status(&mut conn, a, Status::InProgress).unwrap();
    store::set_status(&mut conn, b, Status::OnHold).unwrap();

    let plan =
        planning::commit_session(&mut conn, date("2026-09-07"), [Some(a), Some(b), None]).unwrap();

    assert_eq!(shape(&plan), vec![(a, 1, false), (b, 2, false)]);
    assert_eq!(
        status_of(&conn, a),
        Status::InProgress,
        "picking does not change status"
    );
    assert_eq!(status_of(&conn, b), Status::OnHold);
}

#[test]
fn candidates_are_in_daily_order_and_annotated_with_prior_priority() {
    let (_dir, mut conn) = fresh_db();
    let a = task(&conn, "a");
    let b = task(&conn, "b");
    let c = task(&conn, "c");

    planning::commit_session(&mut conn, date("2026-09-06"), [Some(a), Some(b), None]).unwrap();

    let candidates = planning::candidates(&conn, date("2026-09-07")).unwrap();
    let annotated: Vec<(TaskId, Option<i64>)> = candidates
        .iter()
        .map(|candidate| (candidate.row.task.id, candidate.prior_priority))
        .collect();

    // All three are `new`, so daily order is creation order: a, b, c.
    assert_eq!(annotated, vec![(a, Some(1)), (b, Some(2)), (c, None)]);
}

#[test]
fn a_top_three_task_completed_midday_keeps_history_but_leaves_active_views() {
    let (_dir, mut conn) = fresh_db();
    let a = task(&conn, "a");
    let b = task(&conn, "b");
    let c = task(&conn, "c");

    planning::commit_session(&mut conn, date("2026-09-07"), [Some(a), Some(b), Some(c)]).unwrap();
    store::set_status(&mut conn, a, Status::Done).unwrap();

    // The historical planning_items row for a survives.
    let session = store::planning_session_on(&conn, "2026-09-07")
        .unwrap()
        .unwrap();
    let items = store::planning_items(&conn, session.id).unwrap();
    assert_eq!(items.len(), 3);
    assert!(
        items
            .iter()
            .any(|item| item.task_id == a && item.priority == 1)
    );

    // a is gone from the next day's candidates and is not carried over.
    let candidates = planning::candidates(&conn, date("2026-09-08")).unwrap();
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.row.task.id != a)
    );

    let plan = planning::commit_session(&mut conn, date("2026-09-08"), [None, None, None]).unwrap();
    assert_eq!(
        plan.items
            .iter()
            .map(|item| (item.row.task.id, item.priority))
            .collect::<Vec<_>>(),
        vec![(b, 4), (c, 5)],
        "only the still-open top-3 tasks carry over",
    );
}

#[test]
fn plan_for_returns_the_committed_plan_or_none() {
    let (_dir, mut conn) = fresh_db();
    assert!(
        planning::plan_for(&conn, date("2026-09-07"))
            .unwrap()
            .is_none()
    );

    let a = task(&conn, "a");
    planning::commit_session(&mut conn, date("2026-09-07"), [Some(a), None, None]).unwrap();

    let plan = planning::plan_for(&conn, date("2026-09-07"))
        .unwrap()
        .unwrap();
    assert_eq!(shape(&plan), vec![(a, 1, false)]);
}
