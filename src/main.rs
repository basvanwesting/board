use std::io::{self, Write};
use std::process::ExitCode;

use board::{Board, BriefRow, Env, Error, Exit, ItemId, ListKind, ListRow, Snapshot, Status};
use clap::{Parser, Subcommand, ValueEnum};

/// A file-based board for a planner/worker pair of coding agents and their
/// user. Without a command: the board.
#[derive(Parser)]
#[command(name = "board", disable_help_subcommand = true)]
struct Cli {
    /// The board of this project, not of the repository the command runs in
    /// (also: BOARD)
    #[arg(long, global = true, value_name = "NAME")]
    board: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// List the open briefs, the chores and the backlog, or one of the three
    Ls {
        kind: Option<Kind>,
        /// The briefs with this status; `filed` lists the filed ones
        #[arg(long, value_name = "STATUS")]
        status: Option<Status>,
    },
    /// Print a brief, or the line of a chore or backlog item
    Show {
        /// Like 128, c12 or b41
        id: ItemId,
    },
    /// Check the board's files: `ok`, or a line per finding
    Lint,
}

#[derive(Clone, Copy, ValueEnum)]
enum Kind {
    Briefs,
    Chores,
    Backlog,
}

/// What `ls` lists. `--status` goes with briefs only, and after this type
/// nothing can ask for the chores of a status.
enum Selection {
    /// The three blocks, each under its head.
    Board,
    Briefs(Option<Status>),
    List(ListKind),
}

impl Selection {
    fn new(kind: Option<Kind>, status: Option<Status>) -> Result<Selection, Error> {
        match (kind, status) {
            (None, None) => Ok(Selection::Board),
            (None | Some(Kind::Briefs), status) => Ok(Selection::Briefs(status)),
            (Some(Kind::Chores), None) => Ok(Selection::List(ListKind::Chores)),
            (Some(Kind::Backlog), None) => Ok(Selection::List(ListKind::Backlog)),
            (Some(Kind::Chores | Kind::Backlog), Some(_)) => Err(Error::Usage(
                "--status is for briefs; chores and backlog items have no status".to_owned(),
            )),
        }
    }
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => return usage(&error),
    };
    let (out, exit) = match run(cli) {
        Ok(result) => result,
        Err(error) => {
            complain(&error.to_string());
            return error.exit().into();
        }
    };
    // One checked write, never `println!`: that one panics when the reader
    // has gone (`board ls | head -1`). A reader that stopped reading has
    // what it wanted, so a closed pipe is a quiet success.
    let mut stdout = io::stdout().lock();
    match stdout.write_all(&out).and_then(|()| stdout.flush()) {
        Ok(()) => exit.into(),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Exit::Ok.into(),
        Err(error) => {
            complain(&format!("cannot write the output: {error}"));
            Exit::Failure.into()
        }
    }
}

/// One line on stderr. That it arrives is not checked: there is nowhere left
/// to say that it did not.
fn complain(message: &str) {
    let _ = writeln!(io::stderr().lock(), "board: {message}");
}

/// What clap makes of a command line it does not run: the help, or an error.
fn usage(error: &clap::Error) -> ExitCode {
    if !error.use_stderr() {
        // `--help`, on stdout; clap ignores a closed pipe itself.
        let _ = error.print();
        return Exit::Ok.into();
    }
    // clap's own text (what is wrong, the usage line), under the prefix
    // every other message of the tool has.
    let text = error.render().to_string();
    let text = text.strip_prefix("error: ").unwrap_or(&text);
    complain(text.trim_end());
    Exit::Usage.into()
}

/// The command's stdout and exit code.
fn run(cli: Cli) -> Result<(Vec<u8>, Exit), Error> {
    // What is wrong with the command line comes before what is wrong with
    // the machine.
    let command = match cli.command {
        None => Run::Ls(Selection::Board),
        Some(Command::Ls { kind, status }) => Run::Ls(Selection::new(kind, status)?),
        Some(Command::Show { id }) => Run::Show(id),
        Some(Command::Lint) => Run::Lint,
    };
    let board = Board::open(Env::from_process(cli.board)?)?;
    match command {
        Run::Ls(selection) => Ok((ls(&board.snapshot()?, &selection), Exit::Ok)),
        Run::Show(id) => Ok((board.show(id)?, Exit::Ok)),
        Run::Lint => {
            let findings = board.lint()?;
            if findings.is_empty() {
                Ok((b"ok\n".to_vec(), Exit::Ok))
            } else {
                let lines: String = findings
                    .iter()
                    .map(|finding| format!("{finding}\n"))
                    .collect();
                Ok((lines.into_bytes(), Exit::Refused))
            }
        }
    }
}

/// A command line that says what to do.
enum Run {
    Ls(Selection),
    Show(ItemId),
    Lint,
}

fn ls(snapshot: &Snapshot, selection: &Selection) -> Vec<u8> {
    match selection {
        Selection::Board => [
            b"# briefs\n".to_vec(),
            brief_lines(snapshot.briefs(None)),
            b"\n# chores\n".to_vec(),
            list_lines(snapshot.list(ListKind::Chores)),
            b"\n# backlog\n".to_vec(),
            list_lines(snapshot.list(ListKind::Backlog)),
        ]
        .concat(),
        Selection::Briefs(status) => brief_lines(snapshot.briefs(*status)),
        Selection::List(kind) => list_lines(snapshot.list(*kind)),
    }
}

fn brief_lines<'a>(rows: impl Iterator<Item = &'a BriefRow>) -> Vec<u8> {
    rows.map(|row| format!("{} {} {}\n", row.number, row.status_word(), row.slug))
        .collect::<String>()
        .into_bytes()
}

/// Bytes, not text: a list line is shown as the file has it, UTF-8 or not.
fn list_lines(rows: &[ListRow]) -> Vec<u8> {
    let mut lines = Vec::new();
    for row in rows {
        match row {
            ListRow::Group(name) => {
                lines.extend_from_slice(b"## ");
                lines.extend_from_slice(name);
            }
            ListRow::Item(text) => lines.extend_from_slice(text),
        }
        lines.push(b'\n');
    }
    lines
}
