use std::io::{self, IsTerminal, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::ExitCode;

use board::{
    BacklogId, Board, Body, BriefId, BriefRow, CheckChange, ChoreId, CommitCheck, DEFAULT_GROUP,
    EmptyText, Env, Error, Exit, ItemId, Line, ListId, ListKind, ListRow, NewBacklog, NewChore,
    Shown, Slug, Snapshot, Text,
};
use clap::{ArgGroup, Parser, Subcommand, ValueEnum};

/// A file-based board for a planner/worker pair of coding agents and their
/// user. Without a command: the board, as a TUI on a terminal.
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
        /// The briefs shown with this word: a status, or `unpushed` (filed,
        /// with commits not yet pushed); `filed` lists the other filed ones
        #[arg(long, value_name = "STATUS")]
        status: Option<Shown>,
    },
    /// Print a brief, or a chore or backlog item: its line and its body
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
    /// Stop on a started brief with a question, put under ## Questions
    Block {
        /// Like 128
        id: BriefId,
        /// No `## ` line outside a code fence; use `###`
        question: Text,
    },
    /// Append stdin under a brief's ## Amendments (a blocked brief resumes),
    /// or to a chore's or backlog item's body after a blank line: Markdown,
    /// stored as given
    Amend {
        /// Like 128, c12 or b41
        id: ItemId,
        /// Make stdin the item's body in place of the one it has; empty
        /// stdin removes it. Not for a brief: its amendments are a record of
        /// what was decided when
        #[arg(long)]
        replace: bool,
    },
    /// Make stdin the started brief's ## Report
    Report {
        /// Like 128
        id: BriefId,
    },
    /// File a reported brief: check its commits, delete the backlog items it
    /// closes, move it to briefs/done/
    File {
        /// Like 128
        id: BriefId,
        /// The brief changed nothing in the repository: no commit with its
        /// `Brief: <id>` trailer is asked for
        #[arg(long)]
        no_commits: bool,
    },
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
    /// Change a chore's or backlog item's title, a chore's check, or both.
    /// Its id, group, reference, body and other keys stay
    // The group alone would print `<TEXT|--check <CHECK>> <ID>`, as if
    // the text came first.
    #[command(
        group(ArgGroup::new("change").required(true).multiple(true).args(["text", "check"])),
        override_usage = "board retitle <ID> [TEXT] [--check <CHECK>]"
    )]
    Retitle {
        /// Like c12 or b41
        id: ItemId,
        /// The new title
        text: Option<Line>,
        /// A chore's new check, how to see that it is done; "" removes it
        #[arg(long)]
        check: Option<String>,
    },
    /// Put chores and backlog items into a group, in one commit: a new
    /// name makes a new group, and one no item names is gone
    Group {
        /// Like c12 or b41
        #[arg(required = true)]
        ids: Vec<ListId>,
        /// The group's name
        #[arg(long, value_name = "NAME")]
        to: Line,
    },
    /// Bring the board-data clone and the remote to the same commit
    Sync,
    /// Convert the project's chores.md and backlog.md, the storage before item files, to a file per item, and bring the item files up to date (a `group:` line on each, no `order:`), in one write; while a list file is there every other command is refused on the project
    Migrate,
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
    /// An action of the user's, in a group, with how to see that it is done
    /// when its title does not show it
    Chore {
        text: Line,
        /// The group it is listed under; a new name makes a new group
        #[arg(long, default_value = DEFAULT_GROUP)]
        group: Line,
        /// How to see that it is done, when the title does not show it
        #[arg(long)]
        check: Option<Line>,
        /// The item it came from, like 128
        #[arg(long, value_name = "ID")]
        from: Option<ItemId>,
        /// Read the chore's body from stdin: Markdown, stored as given
        #[arg(long)]
        body: bool,
    },
    /// An item for later, in a group
    Backlog {
        text: Line,
        /// The group it is listed under; a new name makes a new group
        #[arg(long, default_value = DEFAULT_GROUP)]
        group: Line,
        /// The item it came from, like 128
        #[arg(long, value_name = "ID")]
        from: Option<ItemId>,
        /// Read the item's body from stdin: Markdown, stored as given
        #[arg(long)]
        body: bool,
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
    Briefs(Option<Shown>),
    List(ListKind),
}

impl Selection {
    fn new(kind: Option<Kind>, status: Option<Shown>) -> Result<Selection, Error> {
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
        // The plain board when piped: what an agent or a script reads.
        None if io::stdout().is_terminal() => Run::Tui,
        None => Run::Ls(Selection::Board),
        Some(Command::Ls { kind, status }) => Run::Ls(Selection::new(kind, status)?),
        Some(Command::Show { id }) => Run::Show(id),
        Some(Command::Lint) => Run::Lint,
        Some(Command::New {
            item: NewItem::Brief { slug },
        }) => Run::NewBrief(slug),
        Some(Command::Queue { id, closes }) => Run::Queue(id, closes),
        Some(Command::Next) => Run::Next,
        Some(Command::Block { id, question }) => Run::Block(id, question),
        Some(Command::Amend { id, replace }) => amend(id, replace)?,
        Some(Command::Report { id }) => Run::Report(id, stdin_text(&format!("report {id}"))?),
        Some(Command::File { id, no_commits }) => Run::File(
            id,
            if no_commits {
                CommitCheck::Skip
            } else {
                CommitCheck::Trailer
            },
        ),
        Some(Command::Add {
            item:
                AddItem::Chore {
                    text,
                    group,
                    check,
                    from,
                    body,
                },
        }) => Run::AddChore(NewChore {
            text,
            group,
            check,
            from,
            body: stdin_body(body, "add chore")?,
        }),
        Some(Command::Add {
            item:
                AddItem::Backlog {
                    text,
                    group,
                    from,
                    body,
                },
        }) => Run::AddBacklog(NewBacklog {
            text,
            group,
            from,
            body: stdin_body(body, "add backlog")?,
        }),
        Some(Command::Done { ids }) => Run::Done(ids),
        Some(Command::Drop { ids }) => Run::Drop(ids),
        Some(Command::Group { ids, to }) => Run::Group(ids, to),
        Some(Command::Retitle { id, text, check }) => retitle(id, text, check)?,
        Some(Command::Sync) => Run::Sync,
        Some(Command::Migrate) => Run::Migrate,
    };
    // A note is written as it comes, before the command goes on: `waiting
    // for lock` has to show while the command waits.
    let notes = Box::new(|note: board::Note| complain(&note.to_string()));
    let board = Board::open(Env::from_process(cli.board)?, notes)?;
    let line = |text: String| Ok((format!("{text}\n").into_bytes(), Exit::Ok));
    match command {
        Run::Tui => {
            board::tui::run(board)?;
            Ok((Vec::new(), Exit::Ok))
        }
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
        Run::Block(id, question) => {
            board.block(id, &question)?;
            line(format!("blocked {id}"))
        }
        Run::Amend(id, text) => line(board.amend(id, &text)?),
        Run::AmendItem(id, body, replace) => line(board.amend_item(id, &body, replace)?),
        Run::Report(id, text) => {
            board.report(id, &text)?;
            line(format!("reported {id}"))
        }
        Run::File(id, check) => line(board.file(id, check)?.to_string()),
        Run::AddChore(new) => line(board.add_chore(&new)?.to_string()),
        Run::AddBacklog(new) => line(board.add_backlog(&new)?.to_string()),
        Run::Done(ids) => line(board.done(&ids)?),
        Run::Drop(ids) => line(board.drop_items(&ids)?),
        Run::Group(ids, to) => line(board.group(&ids, &to)?),
        Run::Retitle(id, text, check) => line(board.retitle(id, text.as_ref(), check.as_ref())?),
        Run::Sync => {
            board.sync()?;
            line("synced".to_owned())
        }
        Run::Migrate => line(board.migrate()?),
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
    Tui,
    Ls(Selection),
    Show(ItemId),
    Lint,
    NewBrief(Slug),
    Queue(BriefId, Option<Vec<BacklogId>>),
    Next,
    Block(BriefId, Text),
    Amend(BriefId, Text),
    AmendItem(ListId, Body, bool),
    Report(BriefId, Text),
    File(BriefId, CommitCheck),
    AddChore(NewChore),
    AddBacklog(NewBacklog),
    Done(Vec<ChoreId>),
    Drop(Vec<BacklogId>),
    Group(Vec<ListId>, Line),
    Retitle(ListId, Option<Line>, Option<CheckChange>),
    Sync,
    Migrate,
}

/// `retitle`: what the command line alone rules out is a usage error, before
/// the board is read.
fn retitle(id: ItemId, text: Option<Line>, check: Option<String>) -> Result<Run, Error> {
    let what = format!("retitle {id}");
    let check = check
        .map(|check| {
            if check.trim().is_empty() {
                Ok(CheckChange::Remove)
            } else {
                Line::new(&check)
                    .map(CheckChange::Set)
                    .map_err(|why| Error::Usage(format!("{what}: --check: {why}")))
            }
        })
        .transpose()?;
    match id {
        ItemId::Brief(_) => Err(Error::Usage(format!(
            "{what}: a brief's title is in its file and its slug in the file name; \
             retitle is for a chore or a backlog item"
        ))),
        ItemId::Backlog(_) if check.is_some() => {
            Err(Error::Usage(format!("{what}: a backlog item has no check")))
        }
        ItemId::Chore(chore) => Ok(Run::Retitle(ListId::Chore(chore), text, check)),
        ItemId::Backlog(backlog) => Ok(Run::Retitle(ListId::Backlog(backlog), text, check)),
    }
}

/// `amend` of a brief, or of a chore's or backlog item's body.
fn amend(id: ItemId, replace: bool) -> Result<Run, Error> {
    let what = format!("amend {id}");
    let item = match id {
        ItemId::Brief(_) if replace => {
            return Err(Error::Usage(format!(
                "{what}: --replace is for a chore's or backlog item's body; a brief's \
                 amendments are a record of what was decided when"
            )));
        }
        ItemId::Brief(number) => return Ok(Run::Amend(number, stdin_text(&what)?)),
        ItemId::Chore(chore) => ListId::Chore(chore),
        ItemId::Backlog(backlog) => ListId::Backlog(backlog),
    };
    let body = Body::new(&stdin_bytes()?);
    // An empty replace removes the body; an empty append says nothing.
    if body.is_empty() && !replace {
        return Err(no_text(&what));
    }
    Ok(Run::AmendItem(item, body, replace))
}

/// The body of `add … --body`, which must say something; without the flag
/// nothing reads stdin. A flag and not "stdin when there is some": an
/// agent's stdin may be a pipe that never closes, and on a terminal the
/// command would wait for the user.
fn stdin_body(wanted: bool, what: &str) -> Result<Body, Error> {
    if !wanted {
        return Ok(Body::new(b""));
    }
    let body = Body::new(&stdin_bytes()?);
    if body.is_empty() {
        return Err(no_text(what));
    }
    Ok(body)
}

/// The text of `amend` and `report`, read whole from stdin before anything
/// else happens: it is part of the command line, and a write may run its
/// command more than once.
fn stdin_text(what: &str) -> Result<Text, Error> {
    Text::new(&stdin_bytes()?).map_err(|EmptyText| no_text(what))
}

fn stdin_bytes() -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    io::stdin()
        .lock()
        .read_to_end(&mut bytes)
        .map_err(|source| Error::Io {
            action: "read",
            path: "stdin".into(),
            source,
        })?;
    Ok(bytes)
}

fn no_text(what: &str) -> Error {
    Error::Usage(format!(
        "{what}: no text on stdin, which is where it is read from"
    ))
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
    rows.map(|row| row.ls_line() + "\n")
        .collect::<String>()
        .into_bytes()
}

/// A path as a line of its bytes: the clone may sit below a directory whose
/// name is not UTF-8.
fn path_line(path: &Path) -> Vec<u8> {
    [path.as_os_str().as_bytes(), b"\n"].concat()
}

/// Bytes, not text: an item's line is shown as its file has it, UTF-8 or
/// not.
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

/// SPEC.md: every command the planner skill names must exist as it names it,
/// and so must every command the README names. Each `board` invocation in
/// their code is parsed by the definition above, nothing is run: a renamed
/// command or flag breaks this test, not the first agent that follows the
/// skill or the first reader who follows the README. The README's status
/// table is checked against the status type the same way.
#[cfg(test)]
mod docs {
    use board::Status;
    use clap::CommandFactory;

    use super::Cli;

    const SKILL: &str = include_str!("../skill/my-plan.SKILL.md");
    const README: &str = include_str!("../README.md");

    /// The code in a markdown text: inline spans, and the lines of a fenced
    /// block. A `markdown` fence (the brief template) is prose again.
    fn code(text: &str) -> Vec<&str> {
        let mut code = Vec::new();
        // Some(is markdown) inside a fence.
        let mut fence = None;
        for line in text.lines() {
            if let Some(info) = line.trim_start().strip_prefix("```") {
                fence = match fence {
                    None => Some(matches!(info.trim(), "markdown" | "md")),
                    Some(_) => None,
                };
                continue;
            }
            match fence {
                Some(false) => code.push(line.trim()),
                None | Some(true) => code.extend(line.split('`').skip(1).step_by(2)),
            }
        }
        code
    }

    /// A piece of code that runs `board`: all of it, after a `$ ` prompt, the
    /// end of a pipe (`echo … | board report 1`), or a quoted part (`run
    /// "board next" and …`).
    fn invocations(text: &str) -> Vec<String> {
        let runs_board = |code: &str| code == "board" || code.starts_with("board ");
        let mut found = Vec::new();
        for code in code(text) {
            let whole = code.strip_prefix("$ ").unwrap_or(code);
            found.extend(
                whole
                    .split(" | ")
                    .filter(|part| runs_board(part))
                    .map(str::to_owned),
            );
            found.extend(
                code.split('"')
                    .skip(1)
                    .step_by(2)
                    .filter(|quoted| runs_board(quoted))
                    .map(str::to_owned),
            );
        }
        found
    }

    /// The words a shell would pass: split at blanks, double quotes removed.
    fn words(invocation: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut word: Option<String> = None;
        let mut quoted = false;
        for c in invocation.chars() {
            match c {
                '"' => {
                    quoted = !quoted;
                    word.get_or_insert_with(String::new);
                }
                c if c.is_whitespace() && !quoted => words.extend(word.take()),
                c => word.get_or_insert_with(String::new).push(c),
            }
        }
        words.extend(word);
        words
    }

    /// What a word of the skill may stand for. A placeholder is any value
    /// valid in its slot, so `<id>` tries every kind of id: `board done
    /// <id>` takes a chore, `board file <id>` a brief.
    fn samples(word: &str) -> Vec<&str> {
        match word {
            "<id>" => vec!["128", "c12", "b41"],
            "<slug>" => vec!["fix-login"],
            "<question>" | "…" => vec!["Which table holds the orders?"],
            "<text>" => vec!["Rotate the key"],
            "<group>" => vec!["deploy"],
            "<check>" => vec!["the key's date in the dashboard"],
            "<name>" => vec!["shop"],
            placeholder if placeholder.starts_with('<') && placeholder.ends_with('>') => {
                panic!("no sample value for {placeholder}: add one here")
            }
            literal => vec![literal],
        }
    }

    /// clap's error for an invocation no choice of sample values parses.
    fn parse(invocation: &str) -> Result<(), String> {
        let words = words(invocation);
        let mut argvs: Vec<Vec<&str>> = vec![Vec::new()];
        for word in &words {
            let samples = samples(word);
            argvs = argvs
                .iter()
                .flat_map(|argv| {
                    samples
                        .iter()
                        .map(|sample| [argv.as_slice(), &[*sample]].concat())
                })
                .collect();
        }
        let mut error = String::new();
        for argv in argvs {
            match Cli::command().try_get_matches_from(argv) {
                Ok(_) => return Ok(()),
                // `--help` and the like: not an error, main prints it.
                Err(help) if !help.use_stderr() => return Ok(()),
                Err(wrong) => error = wrong.render().to_string(),
            }
        }
        Err(error)
    }

    /// Code that starts with a flag: no invocation carries it, so `parse`
    /// never sees it. The skill writes each flag inside its `board` command.
    fn bare_flags(text: &str) -> Vec<&str> {
        code(text)
            .into_iter()
            .filter(|code| code.starts_with("--"))
            .collect()
    }

    /// What the check refuses, with why: invocations that do not parse, and
    /// flags it cannot check.
    fn failing(text: &str) -> Vec<(String, String)> {
        invocations(text)
            .into_iter()
            .filter_map(|invocation| parse(&invocation).err().map(|error| (invocation, error)))
            .chain(bare_flags(text).into_iter().map(|flag| {
                let why = "unchecked: write it inside its `board` command".to_owned();
                (flag.to_owned(), why)
            }))
            .collect()
    }

    /// The code `failing` refuses, without the reasons.
    fn failing_code(text: &str) -> Vec<String> {
        failing(text).into_iter().map(|(code, _)| code).collect()
    }

    #[test]
    fn every_board_command_the_skill_names_parses() {
        let found = invocations(SKILL);
        // Counted by reading the skill: 26 code spans run `board`, one of
        // them inside quotes. Fewer found means the reading lost some.
        assert!(found.len() >= 26, "found only {}: {found:#?}", found.len());
        let wrong: Vec<String> = failing(SKILL)
            .into_iter()
            .map(|(code, why)| format!("{code}\n{why}"))
            .collect();
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn a_misnamed_command_in_the_skill_fails_the_check() {
        for (from, to, wrong) in [
            (
                "`board ls briefs --status reported`",
                "`board list briefs --status reported`",
                "board list briefs --status reported",
            ),
            ("run \"board next\"", "run \"board nxt\"", "board nxt"),
            (
                "`board queue <id> --closes b41,b52`",
                "`board queue <id> --close b41,b52`",
                "board queue <id> --close b41,b52",
            ),
            (
                "`board queue <id> --closes b41,b52`",
                "`board queue <id>` (`--closes b41,b52`)",
                "--closes b41,b52",
            ),
            (
                "`board done <id>`",
                "`board done <id> <slug>`",
                "board done <id> <slug>",
            ),
        ] {
            let text = SKILL.replacen(from, to, 1);
            assert!(text != SKILL, "the skill no longer has {from}");
            assert_eq!(failing_code(&text), [wrong]);
        }
        let text = format!("{SKILL}\n```sh\n$ board sycn\n```\n");
        assert_eq!(failing_code(&text), ["board sycn"]);
    }

    #[test]
    fn every_board_command_the_readme_names_parses() {
        let found = invocations(README);
        // Counted by reading the README: 44 code spans run `board`, two of
        // them at the end of a pipe. Fewer found means the reading lost some.
        assert!(found.len() >= 44, "found only {}: {found:#?}", found.len());
        let wrong: Vec<String> = failing(README)
            .into_iter()
            .map(|(code, why)| format!("{code}\n{why}"))
            .collect();
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn a_misnamed_command_in_the_readme_fails_the_check() {
        for (from, to, wrong) in [
            (
                "`board next`, `board amend <id>`",
                "`board nxt`, `board amend <id>`",
                "board nxt",
            ),
            (
                "| board amend 1",
                "| board amend 1 --resume",
                "board amend 1 --resume",
            ),
            (
                "$ board queue 1 --closes b1",
                "$ board queue 1 --close b1",
                "board queue 1 --close b1",
            ),
        ] {
            let text = README.replacen(from, to, 1);
            assert!(text != README, "the README no longer has {from}");
            assert_eq!(failing_code(&text), [wrong]);
        }
    }

    /// The statuses the README's table has a row for: the first cell, in
    /// backticks, of each row in the section on a brief's life.
    fn statuses_in_table(text: &str) -> Vec<&str> {
        let section = text
            .split("\n## ")
            .find(|section| section.starts_with("The life of a brief"))
            .expect("the README has a section on the life of a brief");
        let mut statuses: Vec<&str> = section
            .lines()
            .filter_map(|line| line.strip_prefix('|')?.split('|').next())
            .filter_map(|cell| cell.trim().strip_prefix('`')?.strip_suffix('`'))
            .collect();
        statuses.sort_unstable();
        statuses
    }

    /// `Status::ALL` is the type's own list, kept complete by a match in
    /// `brief.rs`; the README lists no second one to compare with.
    fn known_statuses() -> Vec<&'static str> {
        let mut known = Status::ALL.map(Status::as_str).to_vec();
        known.sort_unstable();
        known
    }

    #[test]
    fn the_readme_has_a_row_for_every_status_and_no_other() {
        assert_eq!(statuses_in_table(README), known_statuses());
    }

    #[test]
    fn a_status_missing_from_the_readme_fails_the_check() {
        let row = README
            .lines()
            .find(|line| line.starts_with("| `blocked` |"))
            .expect("the README has a row for blocked");
        let text = README.replacen(&format!("{row}\n"), "", 1);
        assert_ne!(statuses_in_table(&text), known_statuses());
        let text = README.replacen("| `filed` |", "| `closed` |", 1);
        assert_ne!(statuses_in_table(&text), known_statuses());
    }
}
