// handsets-tui — keyboard-driven Android UI inspector backed by the
// handsets daemon. Standalone binary; `hs tui` spawns us with the right
// --host/--port. We talk the same length-prefixed wire protocol that the
// rest of the CLI uses (see ../handsets-cli/src/main.rs Conn).

mod app;
mod conn;
mod json;
mod model;
mod ui;

use std::io::{self, stdout, Stdout};
use std::process::ExitCode;

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::App;

const USAGE: &str = "\
Usage: handsets-tui [options]

Options:
  --host H     daemon host (default 127.0.0.1)
  --port P     daemon port (default 9008)
  -h, --help   show this message
";

struct Args {
    host: String,
    port: u16,
}

impl Default for Args {
    fn default() -> Self { Self { host: "127.0.0.1".into(), port: 9008 } }
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => { print!("{USAGE}"); std::process::exit(0); }
            "--host" => a.host = it.next().ok_or("--host needs a value")?,
            "--port" => a.port = it.next().ok_or("--port needs a value")?
                .parse().map_err(|_| "bad --port".to_string())?,
            other => return Err(format!("unknown arg: {other}")),
        }
    }
    Ok(a)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => { eprintln!("error: {e}\n\n{USAGE}"); return ExitCode::from(2); }
    };

    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => { eprintln!("handsets-tui: {e}"); ExitCode::from(1) }
    }
}

fn run(args: Args) -> io::Result<()> {
    let mut app = App::new(args.host, args.port)?;

    let mut terminal = setup_terminal()?;
    let result = app.run(&mut terminal);
    restore_terminal(&mut terminal)?;
    result
}

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    Terminal::new(CrosstermBackend::new(out))
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}
