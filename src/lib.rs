//! nextup — a keyboard-driven terminal to-do manager.
//!
//! The crate is split into a library and a thin binary. The library owns all the
//! testable pieces: the task model, the SQLite-backed store, the daily planning
//! rules, and the terminal-independent application state. The binary (`main.rs`)
//! only sets up the terminal and runs the event loop.

pub mod app;
pub mod event;
pub mod model;
pub mod planning;
pub mod store;
pub mod ui;
