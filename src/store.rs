//! SQLite persistence: opening a connection with `PRAGMA foreign_keys = ON`,
//! running migrations on startup, and every repository function the rest of the
//! app uses (task CRUD, subtree/ancestor CTEs, status changes, list queries).

// Populated in Phase 1 (CRUD + migrations) and Phase 2 (status + list queries).
