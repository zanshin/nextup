//! Binary entry point: hand-rolled argument parsing, a panic hook that restores
//! the terminal, an RAII terminal guard, and the blocking event loop. Domain
//! logic lives in the `nextup` library crate, not here.

use std::io;
use std::path::PathBuf;

use anyhow::{Context, Result};
use crossterm::event::{Event, KeyEventKind, read};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use directories::ProjectDirs;
use nextup::app::App;
use nextup::{event, store, ui};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

/// Resolved configuration for a run.
struct Args {
    /// Path to the SQLite database file. Created on first run.
    db_path: PathBuf,
}

/// What `parse_args` decided we should do.
enum ArgsOutcome {
    Run(Args),
    Help,
    Version,
}

fn main() -> anyhow::Result<()> {
    match parse_args(std::env::args().skip(1))? {
        ArgsOutcome::Help => print_help(),
        ArgsOutcome::Version => print_version(),
        ArgsOutcome::Run(args) => run(args)?,
    }

    Ok(())
}

/// Open the database, take over the terminal, and run the event loop until the
/// app asks to quit. The `TerminalGuard` restores the terminal on every exit
/// path, including a panic.
fn run(args: Args) -> Result<()> {
    install_panic_hook();

    let mut conn = store::open(&args.db_path)
        .with_context(|| format!("opening database at {}", args.db_path.display()))?;

    let _guard = TerminalGuard::new().context("taking over the terminal")?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let mut app = App::new(&conn, chrono::Local::now().date_naive())?;
    event_loop(&mut terminal, &mut app, &mut conn)
}

/// Draw, block for one key, translate it, apply it. Repeat until `should_quit`.
fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    conn: &mut rusqlite::Connection,
) -> Result<()> {
    while !app.should_quit {
        terminal.draw(|frame| ui::render(app, frame))?;

        if let Event::Key(key) = read()?
            && key.kind != KeyEventKind::Release
            && let Some(action) = event::action_for(key, &app.screen, app.mode())
        {
            app.update(action, conn)?;
        }
    }

    Ok(())
}

/// Parse `--db <path>` / `--db=<path>`, `--help`, and `--version`. Anything else
/// is an error. When no `--db` is given we fall back to `NEXTUP_DB` and then to
/// the platform data directory.
fn parse_args<I: Iterator<Item = String>>(args: I) -> anyhow::Result<ArgsOutcome> {
    let mut db_override: Option<PathBuf> = None;
    let mut args = args;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(ArgsOutcome::Help),
            "-V" | "--version" => return Ok(ArgsOutcome::Version),
            "--db" => {
                let path = args.next().context("--db requires a path argument")?;
                db_override = Some(PathBuf::from(path));
            }
            other if other.starts_with("--db=") => {
                db_override = Some(PathBuf::from(&other["--db=".len()..]));
            }
            other => anyhow::bail!("unrecognized argument: {other}"),
        }
    }

    let db_path = match db_override {
        Some(path) => path,
        None => match std::env::var_os("NEXTUP_DB") {
            Some(value) => PathBuf::from(value),
            None => default_db_path()?,
        },
    };

    Ok(ArgsOutcome::Run(Args { db_path }))
}

/// The default database path, e.g. `~/Library/Application Support/nextup/nextup.db`
/// on macOS. The directory is created lazily by the store on first open.
fn default_db_path() -> anyhow::Result<PathBuf> {
    let dirs = ProjectDirs::from("", "", "nextup")
        .context("could not determine a home directory for the data file")?;

    Ok(dirs.data_dir().join("nextup.db"))
}

fn print_help() {
    println!(
        "nextup {version} — a keyboard-driven terminal to-do manager

USAGE:
    nextup [OPTIONS]

OPTIONS:
        --db <PATH>    Use the SQLite database at <PATH> instead of the default
    -h, --help         Print this help text and exit
    -V, --version      Print version information and exit

The database location defaults to a platform data directory and can be
overridden with the NEXTUP_DB environment variable. The file and its schema are
created automatically on first run.",
        version = env!("CARGO_PKG_VERSION"),
    );
}

fn print_version() {
    println!("nextup {}", env!("CARGO_PKG_VERSION"));
}

/// Install a panic hook that restores the terminal before the default hook runs,
/// so a crash mid-render never leaves the user stuck in raw mode.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();

    std::panic::set_hook(Box::new(move |info| {
        let _ = TerminalGuard::restore();
        default_hook(info);
    }));
}

/// RAII guard for terminal state. `new` enables raw mode and switches to the
/// alternate screen; `Drop` (and the panic hook) undo both, so the terminal is
/// always restored on the way out of `run`.
struct TerminalGuard;

impl TerminalGuard {
    fn new() -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;

        Ok(Self)
    }

    /// Shared teardown used by both `Drop` and the panic hook. Ordering mirrors
    /// `new` in reverse; safe to call even if `new` never ran.
    fn restore() -> io::Result<()> {
        execute!(io::stdout(), LeaveAlternateScreen)?;
        disable_raw_mode()
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = Self::restore();
    }
}
