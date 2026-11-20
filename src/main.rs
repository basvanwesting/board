use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::ExitCode;

use board::{
    BacklogId, Board, BriefId, BriefRow, ChoreId, Env, Error, Exit, ItemId, Line, ListKind,
    ListRow, NewChore, Slug, Snapshot, Status,
};
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
    /// Write a new draft brief's header; prints its path
    New {
        #[command(subcommand)]
        item: NewItem,
    },
    /// Hand a draft brief to the worker
    Queue {
        /// Like 128
        id: BriefId,
        /// The backlog items the brief resolves, deleted when it is filed
        #[arg(long, value_name = "IDS", value_delimiter = ',')]
        closes: Option<Vec<BacklogId>>,
    },
    /// Claim the brief queued longest; prints its path, or `queue empty`
    Next,
    /// Add a chore or a backlog item; prints its new id
    Add {
        #[command(subcommand)]
        item: AddItem,
    },
    /// Delete chores that are done
    Done {
        /// Like c12
        #[arg(required = true)]
        ids: Vec<ChoreId>,
    },
    /// Delete backlog items that will not be done
    Drop {
        /// Like b41
        #[arg(required = true)]
        ids: Vec<BacklogId>,
    },
    /// Bring the board-data clone and the remote to the same commit
    Sync,
}

#[derive(Subcommand)]
enum NewItem {
    /// A brief, numbered after every brief the project ever had
    Brief {
        /// Like fix-login: a-z and 0-9, words joined by dashes
        slug: Slug,
    },
}

#[derive(Subcommand)]
enum AddItem {
    /// An action of the user's, with how to see that it is done
    Chore {
        text: Line,
        /// The `## group` heading it goes under; made when missing
        #[arg(long)]
        group: Line,
        /// How to see that it is done
        #[arg(long)]
        check: Line,
        /// The item it came from, like 128
        #[arg(long, value_name = "ID")]
        from: Option<ItemId>,
    },
    /// An item for later, below the others
    Backlog {
        text: Line,
        /// The item it came from, like 128
        #[arg(long, value_name = "ID")]
        from: Option<ItemId>,
    },
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
        Some(Command::New {
            item: NewItem::Brief { slug },
        }) => Run::NewBrief(slug),
        Some(Command::Queue { id, closes }) => Run::Queue(id, closes),
        Some(Command::Next) => Run::Next,
        Some(Command::Add {
            item:
                AddItem::Chore {
                    text,
                    group,
                    check,
                    from,
                },
        }) => Run::AddChore(NewChore {
            text,
            group,
            check,
            from,
        }),
        Some(Command::Add {
            item: AddItem::Backlog { text, from },
        }) => Run::AddBacklog(text, from),
        Some(Command::Done { ids }) => Run::Done(ids),
        Some(Command::Drop { ids }) => Run::Drop(ids),
        Some(Command::Sync) => Run::Sync,
    };
    // A note is written as it comes, before the command goes on: `waiting
    // for lock` has to show while the command waits.
    let notes = Box::new(|note: board::Note| complain(&note.to_string()));
    let board = Board::open(Env::from_process(cli.board)?, notes)?;
    let line = |text: String| Ok((format!("{text}\n").into_bytes(), Exit::Ok));
    match command {
        Run::Ls(selection) => Ok((ls(&board.snapshot()?, &selection), Exit::Ok)),
        Run::Show(id) => Ok((board.show(id)?, Exit::Ok)),
        Run::NewBrief(slug) => Ok((path_line(&board.new_brief(&slug)?), Exit::Ok)),
        Run::Queue(id, closes) => {
            board.queue(id, closes.as_deref())?;
            line(format!("queued {id}"))
        }
        Run::Next => match board.next()? {
            Some(path) => Ok((path_line(&path), Exit::Ok)),
            None => line("queue empty".to_owned()),
        },
        Run::AddChore(new) => line(board.add_chore(&new)?.to_string()),
        Run::AddBacklog(text, from) => line(board.add_backlog(&text, from)?.to_string()),
        Run::Done(ids) => line(board.done(&ids)?),
        Run::Drop(ids) => line(board.drop_items(&ids)?),
        Run::Sync => {
            board.sync()?;
            line("synced".to_owned())
        }
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
    NewBrief(Slug),
    Queue(BriefId, Option<Vec<BacklogId>>),
    Next,
    AddChore(NewChore),
    AddBacklog(Line, Option<ItemId>),
    Done(Vec<ChoreId>),
    Drop(Vec<BacklogId>),
    Sync,
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

/// A path as a line of its bytes: the clone may sit below a directory whose
/// name is not UTF-8.
fn path_line(path: &Path) -> Vec<u8> {
    [path.as_os_str().as_bytes(), b"\n"].concat()
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
