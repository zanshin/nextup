//! Daily planning domain logic: building the candidate list, committing a
//! planning session (top-3 picks plus auto-ranked carry-over), and swapping a
//! pick out of the day's top 3. The ranking maths lives in pure functions; the
//! rest is thin orchestration over `store`. Domain-rule violations surface as
//! `PlanError`.

use std::collections::HashMap;

use chrono::NaiveDate;
use rusqlite::Connection;

use crate::model::{Status, TaskId};
use crate::store::{self, TaskRow};

/// The first priority handed to carried-over tasks: last session's #1 becomes
/// today's #4, #2 -> #5, and so on.
const FIRST_AUTO_PRIORITY: i64 = 4;

/// A daily-planning rule violation. Storage and SQLite failures are folded in
/// transparently so callers can use `?`.
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("task {0} does not exist")]
    UnknownTask(TaskId),

    #[error("task {0} is not outstanding and cannot be planned")]
    NotOutstanding(TaskId),

    #[error("task {0} is already in today's top 3")]
    AlreadyInTopThree(TaskId),

    #[error("slot {0} is not 1, 2, or 3")]
    BadSlot(u8),

    #[error("no planning session exists for {0}")]
    NoPlan(String),

    #[error(transparent)]
    Storage(Box<dyn std::error::Error + Send + Sync>),
}

impl From<anyhow::Error> for PlanError {
    fn from(err: anyhow::Error) -> Self {
        PlanError::Storage(err.into())
    }
}

impl From<rusqlite::Error> for PlanError {
    fn from(err: rusqlite::Error) -> Self {
        PlanError::Storage(Box::new(err))
    }
}

type PlanResult<T> = Result<T, PlanError>;

/// An outstanding task offered for planning, annotated with the priority it held
/// in the most recent prior session (if it had one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub row: TaskRow,
    pub prior_priority: Option<i64>,
}

/// A committed plan for a date: the session id and its items in priority order
/// (1/2/3 picks first, then carried-over 4+).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub session_id: i64,
    pub plan_date: NaiveDate,
    pub items: Vec<PlanItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanItem {
    pub row: TaskRow,
    pub priority: i64,
    pub carried_over: bool,
}

fn fmt_date(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

/// The outstanding list in daily order, each row annotated with its prior-session
/// priority for display. This is also the planning candidate list.
pub fn candidates(conn: &Connection, date: NaiveDate) -> PlanResult<Vec<Candidate>> {
    let prior = prior_priorities(conn, &fmt_date(date))?;

    let candidates = store::outstanding_ordered(conn)?
        .into_iter()
        .map(|row| {
            let prior_priority = prior.get(&row.task.id).copied();
            Candidate {
                row,
                prior_priority,
            }
        })
        .collect();

    Ok(candidates)
}

/// Commit (or re-commit) the plan for `date`. `picks[i]` takes priority `i + 1`;
/// fewer than three is allowed. Tasks that had a priority in the most recent
/// prior session and are still outstanding and not picked today are carried over
/// at 4, 5, 6, … ordered by that prior priority. Re-planning a date clears the
/// previous items first.
pub fn commit_session(
    conn: &mut Connection,
    date: NaiveDate,
    picks: [Option<TaskId>; 3],
) -> PlanResult<Plan> {
    let date_str = fmt_date(date);
    let prior = prior_priorities(conn, &date_str)?;

    let tx = conn.transaction()?;

    let session_id = match store::planning_session_on(&tx, &date_str)? {
        Some(existing) => {
            store::clear_planning_items(&tx, existing.id)?;
            existing.id
        }
        None => store::create_planning_session(&tx, &date_str)?,
    };

    // Picks are slot-indexed, so priority == slot number.
    let mut picked: Vec<TaskId> = Vec::new();
    for (index, slot) in picks.iter().enumerate() {
        let Some(task_id) = *slot else { continue };

        let task = store::get(&tx, task_id)?.ok_or(PlanError::UnknownTask(task_id))?;
        if !task.status.is_outstanding() {
            return Err(PlanError::NotOutstanding(task_id));
        }

        let priority = index as i64 + 1;
        store::upsert_planning_item(&tx, session_id, task_id, priority, false)?;
        store::set_task_priority(&tx, task_id, Some(priority))?;
        picked.push(task_id);
    }

    // Carry-over: prior members still outstanding and not picked today. A member
    // that dropped out only because it completed keeps its `tasks.priority`.
    let mut carried: Vec<CarryOver> = Vec::new();
    for (&task_id, &prior_priority) in &prior {
        if picked.contains(&task_id) {
            continue;
        }
        match store::get(&tx, task_id)? {
            Some(task) if task.status.is_outstanding() => {
                carried.push(CarryOver {
                    task_id,
                    prior_priority,
                });
            }
            _ => {}
        }
    }

    for ranked in rank_carried_over(carried) {
        store::upsert_planning_item(&tx, session_id, ranked.task_id, ranked.priority, true)?;
        store::set_task_priority(&tx, ranked.task_id, Some(ranked.priority))?;
    }

    tx.commit()?;

    load_plan(conn, date)
}

/// Replace the task in `slot` (1/2/3) of `date`'s plan with `new_task`. The
/// benched task goes `on_hold`, is marked carried-over, and moves to the end of
/// the 4+ range; `new_task` takes the freed slot.
pub fn swap_pick(
    conn: &mut Connection,
    date: NaiveDate,
    slot: u8,
    new_task: TaskId,
) -> PlanResult<Plan> {
    if !(1..=3).contains(&slot) {
        return Err(PlanError::BadSlot(slot));
    }

    let date_str = fmt_date(date);
    let session = store::planning_session_on(conn, &date_str)?
        .ok_or_else(|| PlanError::NoPlan(date_str.clone()))?;
    let items = store::planning_items(conn, session.id)?;

    let new = store::get(conn, new_task)?.ok_or(PlanError::UnknownTask(new_task))?;
    if !new.status.is_outstanding() {
        return Err(PlanError::NotOutstanding(new_task));
    }
    if items
        .iter()
        .any(|item| item.priority <= 3 && item.task_id == new_task)
    {
        return Err(PlanError::AlreadyInTopThree(new_task));
    }

    let slot_priority = i64::from(slot);
    let occupant = items
        .iter()
        .find(|item| item.priority == slot_priority)
        .map(|item| item.task_id);
    let priorities: Vec<i64> = items.iter().map(|item| item.priority).collect();
    let benched_priority = next_auto_priority(&priorities);

    let tx = conn.transaction()?;

    if let Some(occupant) = occupant {
        store::set_status_within(&tx, occupant, Status::OnHold)?;
        store::upsert_planning_item(&tx, session.id, occupant, benched_priority, true)?;
        store::set_task_priority(&tx, occupant, Some(benched_priority))?;
    }

    store::upsert_planning_item(&tx, session.id, new_task, slot_priority, false)?;
    store::set_task_priority(&tx, new_task, Some(slot_priority))?;

    tx.commit()?;

    load_plan(conn, date)
}

/// The committed plan for `date`, or `None` if that day was never planned.
pub fn plan_for(conn: &Connection, date: NaiveDate) -> PlanResult<Option<Plan>> {
    match store::planning_session_on(conn, &fmt_date(date))? {
        Some(_) => Ok(Some(load_plan(conn, date)?)),
        None => Ok(None),
    }
}

fn load_plan(conn: &Connection, date: NaiveDate) -> PlanResult<Plan> {
    let date_str = fmt_date(date);
    let session = store::planning_session_on(conn, &date_str)?
        .ok_or_else(|| PlanError::NoPlan(date_str.clone()))?;

    let mut items = Vec::new();
    for item in store::planning_items(conn, session.id)? {
        let row =
            store::task_row(conn, item.task_id)?.ok_or(PlanError::UnknownTask(item.task_id))?;
        items.push(PlanItem {
            row,
            priority: item.priority,
            carried_over: item.carried_over,
        });
    }

    Ok(Plan {
        session_id: session.id,
        plan_date: date,
        items,
    })
}

/// Task -> prior priority for the most recent session strictly before `date_str`.
/// Empty when there is no prior session.
fn prior_priorities(conn: &Connection, date_str: &str) -> PlanResult<HashMap<TaskId, i64>> {
    let Some(session) = store::latest_planning_session_before(conn, date_str)? else {
        return Ok(HashMap::new());
    };

    let map = store::planning_items(conn, session.id)?
        .into_iter()
        .map(|item| (item.task_id, item.priority))
        .collect();

    Ok(map)
}

// --- pure ranking helpers ------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CarryOver {
    task_id: TaskId,
    prior_priority: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AutoRanked {
    task_id: TaskId,
    priority: i64,
}

/// Order carry-over tasks by their prior priority and assign new priorities from
/// `FIRST_AUTO_PRIORITY` upward. `task_id` breaks ties so the result is stable.
fn rank_carried_over(mut carried: Vec<CarryOver>) -> Vec<AutoRanked> {
    carried.sort_by_key(|carry| (carry.prior_priority, carry.task_id.value()));

    carried
        .into_iter()
        .zip(FIRST_AUTO_PRIORITY..)
        .map(|(carry, priority)| AutoRanked {
            task_id: carry.task_id,
            priority,
        })
        .collect()
}

/// The next free priority at the end of the 4+ range, given the priorities
/// already present in a session.
fn next_auto_priority(existing: &[i64]) -> i64 {
    existing
        .iter()
        .copied()
        .max()
        .map_or(FIRST_AUTO_PRIORITY, |max| {
            (max + 1).max(FIRST_AUTO_PRIORITY)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_carried_over_reranks_by_prior_priority_from_four() {
        let carried = vec![
            CarryOver {
                task_id: TaskId(10),
                prior_priority: 2,
            },
            CarryOver {
                task_id: TaskId(11),
                prior_priority: 1,
            },
            CarryOver {
                task_id: TaskId(12),
                prior_priority: 5,
            },
        ];

        assert_eq!(
            rank_carried_over(carried),
            vec![
                AutoRanked {
                    task_id: TaskId(11),
                    priority: 4,
                },
                AutoRanked {
                    task_id: TaskId(10),
                    priority: 5,
                },
                AutoRanked {
                    task_id: TaskId(12),
                    priority: 6,
                },
            ],
        );
    }

    #[test]
    fn rank_carried_over_handles_an_empty_set() {
        assert_eq!(rank_carried_over(vec![]), vec![]);
    }

    #[test]
    fn rank_carried_over_breaks_prior_priority_ties_by_task_id() {
        let carried = vec![
            CarryOver {
                task_id: TaskId(20),
                prior_priority: 3,
            },
            CarryOver {
                task_id: TaskId(9),
                prior_priority: 3,
            },
        ];

        assert_eq!(
            rank_carried_over(carried),
            vec![
                AutoRanked {
                    task_id: TaskId(9),
                    priority: 4,
                },
                AutoRanked {
                    task_id: TaskId(20),
                    priority: 5,
                },
            ],
        );
    }

    #[test]
    fn next_auto_priority_starts_at_four_and_appends_past_the_max() {
        assert_eq!(next_auto_priority(&[]), 4);
        assert_eq!(next_auto_priority(&[1, 2, 3]), 4);
        assert_eq!(next_auto_priority(&[1, 2, 3, 4, 5]), 6);
        assert_eq!(next_auto_priority(&[1, 2, 3, 7]), 8);
    }
}
