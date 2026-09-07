//! Core task types: the `Task` record, its `Status`, and the `TaskId` newtype.
//! Also owns the mapping between `Status` values and their stored text form,
//! which must stay in lock-step with the CHECK constraint in migration 1.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};

/// Lifecycle state of a task. The text forms are the values persisted in the
/// `tasks.status` column and enforced by its CHECK constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    New,
    InProgress,
    OnHold,
    Waiting,
    Abandoned,
    Done,
}

impl Status {
    /// Every variant, in schema order. Handy for exhaustive tests and pickers.
    pub const ALL: [Status; 6] = [
        Status::New,
        Status::InProgress,
        Status::OnHold,
        Status::Waiting,
        Status::Abandoned,
        Status::Done,
    ];

    /// The stored text form, e.g. `Status::InProgress` <-> `"in_progress"`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::New => "new",
            Status::InProgress => "in_progress",
            Status::OnHold => "on_hold",
            Status::Waiting => "waiting",
            Status::Abandoned => "abandoned",
            Status::Done => "done",
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Status {
    type Err = UnknownStatus;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "new" => Ok(Status::New),
            "in_progress" => Ok(Status::InProgress),
            "on_hold" => Ok(Status::OnHold),
            "waiting" => Ok(Status::Waiting),
            "abandoned" => Ok(Status::Abandoned),
            "done" => Ok(Status::Done),
            other => Err(UnknownStatus(other.to_owned())),
        }
    }
}

/// A status text that does not correspond to any `Status` variant. Only reachable
/// if the database holds a value the CHECK constraint should have rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown task status: {0:?}")]
pub struct UnknownStatus(pub String);

/// A task's primary key. Newtype so a raw row id can't be mixed up with any
/// other `i64` (priority, parent id passed positionally, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskId(pub i64);

impl TaskId {
    pub fn value(self) -> i64 {
        self.0
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A single task row. `notes` is always a string (empty, never NULL, per the
/// schema default). `completed_at` is set iff `status` is `Done` or `Abandoned`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: TaskId,
    pub parent_id: Option<TaskId>,
    pub title: String,
    pub notes: String,
    pub status: Status,
    pub priority: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_text_matches_schema_values() {
        // These strings are duplicated in the migration 1 CHECK constraint; if
        // one side changes the other must too.
        assert_eq!(Status::New.as_str(), "new");
        assert_eq!(Status::InProgress.as_str(), "in_progress");
        assert_eq!(Status::OnHold.as_str(), "on_hold");
        assert_eq!(Status::Waiting.as_str(), "waiting");
        assert_eq!(Status::Abandoned.as_str(), "abandoned");
        assert_eq!(Status::Done.as_str(), "done");
    }

    #[test]
    fn status_round_trips_through_text() {
        for status in Status::ALL {
            assert_eq!(status.as_str().parse::<Status>(), Ok(status));
        }
    }

    #[test]
    fn unknown_status_text_is_rejected() {
        assert_eq!(
            "sideways".parse::<Status>(),
            Err(UnknownStatus("sideways".to_owned())),
        );
    }

    #[test]
    fn task_id_displays_as_its_number() {
        assert_eq!(TaskId(42).to_string(), "42");
        assert_eq!(TaskId(42).value(), 42);
    }
}
