//! Which project a command is about, where its files sit in the board-data
//! repo, and the files as read.

use std::collections::BTreeSet;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use crate::brief::{BadStatus, Brief, Stamp, Status, Transition, TransitionError};
use crate::error::Error;
use crate::git::{self, Git, Oid};
use crate::id::{BriefId, ItemId, ListId, ListKind};
use crate::item::{Item, Items};
use crate::list::FindError;

/// What a command takes from the process it runs in. `main` fills it once;
/// nothing below reads the environment or the current directory again, so a
/// caller that builds one by hand decides all of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Env {
    /// `$HOME`: every project is checked out below `home/Work`.
    pub home: PathBuf,
    /// The board-data clone: `$BOARD_ROOT`, else `home/Work/board-data`.
    pub root: PathBuf,
    pub cwd: PathBuf,
    /// `--board`, else `$BOARD`: the project's name, in place of the one the
    /// current directory gives. Not checked yet: [`resolve`] does.
    pub board: Option<String>,
}

impl Env {
    /// `board` is the `--board` argument.
    pub fn from_process(board: Option<String>) -> Result<Env, Error> {
        let home = variable("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| Error::Setup("HOME is not set".to_owned()))?;
        let root = variable("BOARD_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("Work").join("board-data"));
        let cwd = env::current_dir().map_err(|source| {
            Error::Setup(format!("cannot tell the current directory: {source}"))
        })?;
        let board = match board {
            Some(name) => Some(name),
            None => variable("BOARD")
                .map(|name| {
                    name.into_string().map_err(|name| {
                        Error::Usage(not_a_project_name(&name.to_string_lossy(), "BOARD"))
                    })
                })
                .transpose()?,
        };
        Ok(Env {
            home,
            root,
            cwd,
            board,
        })
    }
}

/// An environment variable that is set to something. An empty one counts as
/// unset, as in a shell's `${VAR:-default}`: `BOARD= board ls` asks for the
/// derived project, not for a project without a name.
fn variable(name: &str) -> Option<OsString> {
    env::var_os(name).filter(|value| !value.is_empty())
}

const PROJECT_GRAMMAR: &str = "[A-Za-z0-9][A-Za-z0-9._-]*";

fn not_a_project_name(name: &str, origin: &str) -> String {
    format!("`{name}` (from {origin}) is not a project name ({PROJECT_GRAMMAR})")
}

/// The project a command is about: the given name, else the one derived from
/// the repository the command runs in.
///
/// Derived: the path of the repository's checkout below `home/Work`, `/`
/// replaced by `-` (`~/Work/shop` is `shop`, `~/Work/tools/shop` is
/// `tools-shop`). So a project needs no marker file and every machine gives
/// the same name, because every machine has its projects at the same place.
///
/// A work tree added with `git worktree add` is on the board of the
/// repository it was added to, wherever it is. Its own path is one session's
/// choice (`~/Work/shop/.claude/worktrees/x`) and mostly a project name too:
/// taken as the project it gives an empty board without a word, and a write
/// there would start a project under that name.
///
/// The board-data clone gives no project, for the same reason: it is a
/// repository below `Work` like the others, and the one where lists are
/// edited by hand, so its path as a name (`board-data`) is an empty board and
/// a lint that says `ok` right after such an edit. It is the clone at
/// `env.root` that is refused, not the name: with the board kept elsewhere,
/// `~/Work/board-data` is a project like any other. Rejected: reading the
/// project from the directory inside the clone (`board-data/shop` is `shop`),
/// a second way to name a project that only this one place would have.
///
/// A given name replaces the name only. It runs no git, so a read with
/// `--board` works anywhere.
pub fn resolve(env: &Env) -> Result<ProjectName, Error> {
    match locate(env)? {
        Located::Project(name) => Ok(name),
        Located::Clone(checkout) => Err(Error::Usage(format!(
            "no project for {}: it is the board-data clone, which holds every project's \
             board; {HINT}",
            checkout.display()
        ))),
    }
}

const HINT: &str = "name the board with --board <name>";

/// Where a command without `--board` runs, as [`resolve`] reads it.
pub(crate) enum Located {
    Project(ProjectName),
    /// The board-data clone's own work tree, at this checkout, or a
    /// directory in it: no project, though `board sync` runs there. A work
    /// tree added to the clone is not this (it is refused as before): a sync
    /// commits the clone's own work tree, so one there would leave the edits
    /// of the added one out and say `synced`.
    Clone(PathBuf),
}

/// [`resolve`], with the board-data clone told apart instead of refused.
pub(crate) fn locate(env: &Env) -> Result<Located, Error> {
    if let Some(name) = &env.board {
        return ProjectName::parse(name)
            .map(Located::Project)
            .ok_or_else(|| Error::Usage(not_a_project_name(name, "--board or BOARD")));
    }
    let git = Git::at(&env.cwd);
    let no_answer = |failure| match failure {
        git::Failure::NotRun(source) => Error::Git(format!("cannot run git: {source}")),
        git::Failure::Said(said) => Error::Usage(format!(
            "no project for {}: {said}; {HINT}",
            env.cwd.display()
        )),
    };
    let toplevel = git.toplevel().map_err(no_answer)?;
    // The checkout that names the project, and the added work tree the
    // command runs in when that is another directory.
    let (checkout, worktree) = match git.added_to().map_err(no_answer)? {
        None => (toplevel, None),
        Some(repository) => {
            // A repository's own checkout is where its `.git` is. Any other
            // git directory (a bare repository, a submodule's) does not say
            // where a checkout is, and a guess is a wrong board.
            let own = repository
                .parent()
                .filter(|_| repository.file_name() == Some(OsStr::new(".git")));
            let Some(own) = own else {
                return Err(Error::Usage(format!(
                    "no project for {}: it is a worktree of {}, which is not the .git of a checkout; {HINT}",
                    toplevel.display(),
                    repository.display()
                )));
            };
            (own.to_owned(), Some(toplevel))
        }
    };
    // Both sides canonical, or a symlink on the way to either (`/tmp` on a
    // Mac, a home on another disk) would put every project outside. A `Work`
    // that is not there stays as it is and then has nothing below it.
    let work = env.home.join("Work");
    let work = fs::canonicalize(&work).unwrap_or(work);
    let checkout =
        fs::canonicalize(&checkout).map_err(|source| Error::io("resolve", &checkout, source))?;
    let added = worktree.is_some();
    let shown = match worktree {
        None => checkout.display().to_string(),
        Some(worktree) => format!(
            "{}, the repository of the worktree {}",
            checkout.display(),
            worktree.display()
        ),
    };
    // Canonical like the checkout. A clone that is not there is no checkout,
    // and `Board::open` says that it is missing.
    let root = fs::canonicalize(&env.root).unwrap_or_else(|_| env.root.clone());
    if checkout == root && !added {
        return Ok(Located::Clone(checkout));
    }
    if checkout == root {
        return Err(Error::Usage(format!(
            "no project for {shown}: it is the board-data clone, which holds every project's \
             board; {HINT}"
        )));
    }
    name_below(&work, &checkout)
        .map(Located::Project)
        .map_err(|why| {
            Error::Usage(match why {
                NoName::Outside => format!(
                    "no project for {shown}: it is not below {}; {HINT}",
                    work.display()
                ),
                NoName::Not(name) => format!("{}; {HINT}", not_a_project_name(&name, &shown)),
            })
        })
}

#[derive(Debug, PartialEq, Eq)]
enum NoName {
    /// The directory is not below `Work`.
    Outside,
    /// The path below `Work` gives this, which is no project name.
    Not(String),
}

fn name_below(work: &Path, checkout: &Path) -> Result<ProjectName, NoName> {
    let below = checkout.strip_prefix(work).map_err(|_| NoName::Outside)?;
    let parts: Vec<_> = below
        .components()
        .map(|part| match part {
            Component::Normal(part) => part.to_string_lossy(),
            // Neither path has these once it is canonical.
            Component::Prefix(_)
            | Component::RootDir
            | Component::CurDir
            | Component::ParentDir => part.as_os_str().to_string_lossy(),
        })
        .collect();
    if parts.is_empty() {
        return Err(NoName::Outside);
    }
    let name = parts.join("-");
    // A part that is not UTF-8 holds U+FFFD now, which no project name does.
    ProjectName::parse(&name).ok_or(NoName::Not(name))
}

/// A project's directory name in the board-data repo:
/// `[A-Za-z0-9][A-Za-z0-9._-]*`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectName(String);

impl ProjectName {
    pub fn parse(name: &str) -> Option<ProjectName> {
        let mut bytes = name.bytes();
        let valid = bytes.next().is_some_and(|b| b.is_ascii_alphanumeric())
            && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        valid.then(|| ProjectName(name.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProjectName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A brief's file name: `<digits>-<one or more chars, no "/">.md`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BriefName {
    number: BriefId,
    file_name: String,
    /// Length of the leading digits; the number alone cannot say, `007` is 7.
    digits: usize,
}

impl BriefName {
    /// The name `new brief` gives: `<number>-<slug>.md`.
    pub fn new(number: BriefId, slug: &Slug) -> BriefName {
        let digits = number.to_string();
        BriefName {
            number,
            file_name: format!("{digits}-{slug}.md"),
            digits: digits.len(),
        }
    }

    pub fn parse(file_name: &str) -> Option<BriefName> {
        let (digits, rest) = file_name.split_once('-')?;
        let slug = rest.strip_suffix(".md")?;
        if slug.is_empty() || slug.contains('/') {
            return None;
        }
        Some(BriefName {
            number: BriefId::parse(digits.as_bytes())?,
            file_name: file_name.to_owned(),
            digits: digits.len(),
        })
    }

    pub fn number(&self) -> BriefId {
        self.number
    }

    /// The name without its number and `.md`.
    pub fn slug(&self) -> &str {
        &self.file_name[self.digits + "-".len()..self.file_name.len() - ".md".len()]
    }

    pub fn as_str(&self) -> &str {
        &self.file_name
    }
}

/// The part of a new brief's file name after its number:
/// `[a-z0-9]+(-[a-z0-9]+)*`, at most [`Slug::MAX`] bytes.
///
/// Narrower than what a brief's name may be (an archived brief keeps the name
/// it had), so that every name the tool writes types and greps the same way.
/// Bounded, because the name has to fit in a file name on every clone: a
/// brief whose file no clone can write would stop every clone's checkout from
/// moving past the commit that added it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slug(String);

impl Slug {
    pub const MAX: usize = 100;
}

impl fmt::Display for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Slug {
    type Err = InvalidSlug;

    fn from_str(s: &str) -> Result<Slug, InvalidSlug> {
        let word = |word: &str| {
            !word.is_empty()
                && word
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        };
        if s.len() <= Slug::MAX && s.split('-').all(word) {
            Ok(Slug(s.to_owned()))
        } else {
            Err(InvalidSlug(s.to_owned()))
        }
    }
}

/// A word that is not a [`Slug`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidSlug(String);

impl fmt::Display for InvalidSlug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`{}` is not a slug: words of a-z and 0-9 joined by single dashes, \
             like fix-login, at most {} characters",
            self.0.escape_debug(),
            Slug::MAX
        )
    }
}

impl std::error::Error for InvalidSlug {}

/// A path the board owns, relative to `$BOARD_ROOT`:
///
/// ```text
/// <project>/chores/c<id>.md | <project>/backlog/b<id>.md
/// <project>/briefs/<brief> | <project>/briefs/parked/<brief> | <project>/briefs/done/<brief>
/// ```
///
/// An item's file is named by its id only (owner, 2026-11-25): a retitle
/// never renames it.
///
/// Anything else in the repo is not the board's: never committed by the tool,
/// never merged.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BoardPath {
    /// A chore's or backlog item's file.
    Item { project: ProjectName, id: ListId },
    Brief {
        project: ProjectName,
        name: BriefName,
        place: Place,
    },
}

/// Where a brief's file sits below `briefs/`. The place decides two
/// statuses whatever the header says: a brief in `done/` is filed, one in
/// `parked/` parked.
///
/// A folder for the parked briefs and not only a status (owner,
/// 2026-11-26): "If we have a folder (planned) it would be much easier to
/// distinguish briefs during daily build cycles. Otherwise the planned
/// state would be interleaved and messy"; "A git move isn't expensive and
/// briefs don't change much".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Place {
    /// `briefs/`.
    Open,
    /// `briefs/parked/`: a finished brief that waits for the owner's word.
    Parked,
    /// `briefs/done/`.
    Done,
}

impl Place {
    pub const ALL: [Place; 3] = [Place::Open, Place::Parked, Place::Done];

    /// The folder below `briefs/`, with its slash; empty for `briefs/`.
    pub fn dir(self) -> &'static str {
        match self {
            Place::Open => "",
            Place::Parked => "parked/",
            Place::Done => "done/",
        }
    }
}

/// Whether `path` is `<project>/chores.md` or `<project>/backlog.md`, a list
/// file of the storage before item files: outside the board's grammar, but
/// committed when changed by hand and kept over a delete in a merge, so
/// that the list `migrate` converts is the one in the files.
pub(crate) fn is_old_list(path: &str) -> bool {
    path.split_once('/').is_some_and(|(project, rest)| {
        ProjectName::parse(project).is_some()
            && ListKind::ALL.iter().any(|kind| rest == kind.file_name())
    })
}

impl BoardPath {
    /// The single path grammar: the worktree scan, the commit of hand edits,
    /// the merge and lint all ask here whether a path is the board's.
    ///
    /// It takes `&str` and no bytes: a file name that is not UTF-8 is not the
    /// board's, like any other name outside the grammar, so no caller has to
    /// carry raw bytes into output that agents read. Every path has one
    /// spelling (`Display` gives it back), so a path can serve as a key.
    pub fn parse(path: &str) -> Option<BoardPath> {
        let (project, rest) = path.split_once('/')?;
        let project = ProjectName::parse(project)?;
        for kind in ListKind::ALL {
            if let Some(name) = rest
                .strip_prefix(kind.dir_name())
                .and_then(|rest| rest.strip_prefix('/'))
            {
                let number = kind.parse_id(name.strip_suffix(".md")?.as_bytes())?;
                return Some(BoardPath::Item {
                    project,
                    id: ListId::of(kind, number)?,
                });
            }
        }
        let brief = rest.strip_prefix("briefs/")?;
        let (place, file_name) = [Place::Done, Place::Parked]
            .into_iter()
            .find_map(|place| Some((place, brief.strip_prefix(place.dir())?)))
            .unwrap_or((Place::Open, brief));
        let name = BriefName::parse(file_name)?;
        Some(BoardPath::Brief {
            project,
            name,
            place,
        })
    }

    /// The brief or item the path is of.
    pub fn item_id(&self) -> ItemId {
        match self {
            BoardPath::Item {
                id: ListId::Chore(id),
                ..
            } => ItemId::Chore(*id),
            BoardPath::Item {
                id: ListId::Backlog(id),
                ..
            } => ItemId::Backlog(*id),
            BoardPath::Brief { name, .. } => ItemId::Brief(name.number()),
        }
    }

    pub fn project(&self) -> &ProjectName {
        match self {
            BoardPath::Item { project, .. } | BoardPath::Brief { project, .. } => project,
        }
    }
}

impl fmt::Display for BoardPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BoardPath::Item { project, id } => {
                write!(f, "{project}/{}/{id}.md", id.kind().dir_name())
            }
            BoardPath::Brief {
                project,
                name,
                place,
            } => write!(f, "{project}/briefs/{}{}", place.dir(), name.as_str()),
        }
    }
}

/// One brief file of a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BriefFile {
    pub name: BriefName,
    pub place: Place,
    pub brief: Brief,
}

/// Where a queued brief stands in the queue, first to last: the briefs
/// put first by `queue --first` (a `first:` counter), the highest counter
/// first; then the rest by their `queued:` time, one without a readable time
/// last; ties by number, which the caller adds. One order for `next`, `ls`
/// and the TUI.
///
/// A counter and not a time (planner, 2026-11-26): a stamp is whole
/// seconds, so two `F` presses in one second would tie and the earlier
/// could win. Rejected: moving a brief first by rewriting `queued:`, a
/// false time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QueuePlace {
    unmarked: bool,
    first: std::cmp::Reverse<u64>,
    untimed: bool,
    queued: Option<Stamp>,
}

/// The header key `queue --first` writes.
pub(crate) const FIRST: &str = "first";

impl BriefFile {
    /// The brief's `first:` counter; `None` without a readable one.
    pub fn first(&self) -> Option<u64> {
        str::from_utf8(self.brief.get(FIRST)?).ok()?.parse().ok()
    }

    pub fn queue_place(&self) -> QueuePlace {
        let first = self.first();
        let queued = self
            .brief
            .get("queued")
            .and_then(|value| str::from_utf8(value).ok()?.parse::<Stamp>().ok());
        QueuePlace {
            unmarked: first.is_none(),
            first: std::cmp::Reverse(first.unwrap_or(0)),
            untimed: queued.is_none(),
            queued,
        }
    }

    /// The brief's status on the board.
    ///
    /// Where the file sits decides whether a brief is filed or parked, not
    /// its header: an archived brief placed in `briefs/done/` by hand has no
    /// header, and a file has one place where a header can say anything. So
    /// `filed` or `parked` in the header of a brief outside its folder is no
    /// status either: nothing moves on from it, and the board would show it
    /// among the open ones.
    pub fn status(&self) -> Result<Status, BadStatus> {
        match self.place {
            Place::Done => return Ok(Status::Filed),
            Place::Parked => return Ok(Status::Parked),
            Place::Open => {}
        }
        match self.brief.status()? {
            status @ (Status::Filed | Status::Parked) => {
                Err(BadStatus::Unknown(status.as_str().to_owned()))
            }
            status @ (Status::Draft
            | Status::Queued
            | Status::Started
            | Status::Blocked
            | Status::Reported) => Ok(status),
        }
    }

    /// Makes the move from the brief's status on the board. Not the header's
    /// alone: a brief in `briefs/done/` is filed whatever its header says,
    /// and no move leaves filed; one in `briefs/parked/` is parked, and
    /// only `queue` moves on from it.
    ///
    /// Filing moves the file to `briefs/done/` too, parking to
    /// `briefs/parked/`, queueing a parked one back to `briefs/`, each in the
    /// same step: a header that says `filed` or `parked` anywhere else is no
    /// status (see [`BriefFile::status`]), so the two cannot be separate
    /// edits. [`Project::changed_paths`] then commits the move.
    pub fn transition(
        &mut self,
        transition: Transition,
        now: Stamp,
    ) -> Result<Status, TransitionError> {
        let from = self.status().map_err(TransitionError::BadStatus)?;
        if transition.apply(from).is_none() {
            return Err(TransitionError::NotFrom(from));
        }
        // From the status on the board, not the header's: a brief placed in
        // `briefs/parked/` by hand with another `status:` is parked, so its
        // release goes, as it lists and lints.
        let to = self.brief.transition_from(from, transition, now)?;
        self.place = match to {
            Status::Filed => Place::Done,
            Status::Parked => Place::Parked,
            Status::Draft
            | Status::Queued
            | Status::Started
            | Status::Blocked
            | Status::Reported => Place::Open,
        };
        Ok(to)
    }

    pub fn path(&self, project: &ProjectName) -> BoardPath {
        BoardPath::Brief {
            project: project.clone(),
            name: self.name.clone(),
            place: self.place,
        }
    }
}

/// A project's board, as its files hold it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub(crate) name: ProjectName,
    pub(crate) chores: Items,
    pub(crate) backlog: Items,
    /// By number, an open brief before a parked one before a filed one,
    /// then by file name.
    pub(crate) briefs: Vec<BriefFile>,
    /// The `.md` files below `briefs/`, `chores/` and `backlog/` that are
    /// outside the path grammar, as paths relative to the board-data root.
    /// The tool does not sync them; lint says so.
    pub(crate) misnamed: Vec<String>,
    /// `<project>/chores.md` and `<project>/backlog.md`, the one file per
    /// kind of the storage before item files, when one is there: only
    /// `migrate` works on such a project ([`Project::refuse_old_lists`]).
    pub(crate) old_lists: Vec<OldList>,
    /// The local versions of briefs and items that lost to the remote's in
    /// a merge (`<path>.local`, `.local2`, …), as paths relative to the
    /// board-data root with what they are of. Untracked, so only a load
    /// from the files has them.
    pub(crate) copies: Vec<(String, ItemId)>,
    /// The files an edit went through since the load: what a write commits.
    pub(crate) changed: BTreeSet<BoardPath>,
}

/// A list file of the storage before item files, as a load found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OldList {
    pub(crate) kind: ListKind,
    /// Relative to the board-data root.
    pub(crate) path: String,
    pub(crate) bytes: Vec<u8>,
    /// Converted by `migrate`: the write deletes it.
    pub(crate) converted: bool,
}

impl OldList {
    fn new(kind: ListKind, path: String, bytes: Vec<u8>) -> OldList {
        OldList {
            kind,
            path,
            bytes,
            converted: false,
        }
    }
}

impl Project {
    /// Refused while the project has a list file of the older storage, for
    /// every command but `migrate`, reads included: this version does not
    /// read the lists, and a board that looks empty, or a write next to
    /// items it cannot see, is what the refusal prevents. A list can also
    /// come back after the migration, from a machine still on the old
    /// binary; `migrate` then folds it in.
    pub(crate) fn refuse_old_lists(&self) -> Result<(), Error> {
        let Some(list) = self.old_lists.first() else {
            return Ok(());
        };
        Err(Error::Refused(format!(
            "{}: a list file of the older storage, which this version does not read; \
             run `board migrate` to convert it to item files",
            list.path
        )))
    }

    /// Whether a write has something to commit.
    pub(crate) fn has_changes(&self) -> bool {
        !self.changed.is_empty() || self.old_lists.iter().any(|list| list.converted)
    }

    /// A project without files, which is also what a project without a
    /// directory is: its board is empty, not missing.
    pub fn empty(name: ProjectName) -> Project {
        Project {
            name,
            chores: Items::new(ListKind::Chores),
            backlog: Items::new(ListKind::Backlog),
            briefs: Vec::new(),
            misnamed: Vec::new(),
            old_lists: Vec::new(),
            copies: Vec::new(),
            changed: BTreeSet::new(),
        }
    }

    pub fn name(&self) -> &ProjectName {
        &self.name
    }

    pub fn list(&self, kind: ListKind) -> &Items {
        match kind {
            ListKind::Chores => &self.chores,
            ListKind::Backlog => &self.backlog,
        }
    }

    pub fn briefs(&self) -> &[BriefFile] {
        &self.briefs
    }

    /// The one brief file with this number, open, parked or filed: a number on two
    /// files names no brief until one of them is renamed.
    pub fn find_brief(&self, number: BriefId) -> Result<&BriefFile, FindError> {
        let mut files = self
            .briefs
            .iter()
            .filter(|file| file.name.number() == number);
        match (files.next(), files.count()) {
            (None, _) => Err(FindError::Missing),
            (Some(file), 0) => Ok(file),
            (Some(_), more) => Err(FindError::Duplicate(more + 1)),
        }
    }

    /// The one brief with this number, to edit; the edit is committed by the
    /// write it runs in.
    pub(crate) fn brief_mut(&mut self, number: BriefId) -> Result<&mut BriefFile, FindError> {
        let path = self.find_brief(number)?.path(&self.name);
        self.changed.insert(path);
        Ok(self
            .briefs
            .iter_mut()
            .find(|file| file.name.number() == number)
            .expect("found above"))
    }

    /// Adds a brief file; the write it runs in commits it.
    pub(crate) fn create_brief(&mut self, file: BriefFile) {
        self.changed.insert(file.path(&self.name));
        self.briefs.push(file);
        self.briefs.sort_by(brief_order);
    }

    /// A kind's items to edit: [`Project::touched`] then names the files the
    /// edit changed, which the write commits.
    pub(crate) fn list_mut(&mut self, kind: ListKind) -> &mut Items {
        match kind {
            ListKind::Chores => &mut self.chores,
            ListKind::Backlog => &mut self.backlog,
        }
    }

    /// Records that the file of item `id` changed (or is gone): the write
    /// commits it.
    pub(crate) fn touched(&mut self, id: ListId) {
        self.changed.insert(self.item_path(id));
    }

    /// The path of item `id` in this project, whether it is there or not.
    pub fn item_path(&self, id: ListId) -> BoardPath {
        BoardPath::Item {
            project: self.name.clone(),
            id,
        }
    }

    /// The item with this id, to edit; the edit is committed by the write
    /// it runs in.
    pub(crate) fn item_mut(&mut self, id: ListId) -> Option<&mut Item> {
        self.list(id.kind()).get(id.get())?;
        self.touched(id);
        self.list_mut(id.kind()).get_mut(id.get())
    }

    /// The paths a write commits: every file an edit went through, and
    /// where each brief among them is now. A brief filed since the load has
    /// moved to `briefs/done/`: its old path is deleted, the new one
    /// written. Asked here rather than recorded at the move, so no edit can
    /// move a brief without its new path in the commit.
    pub(crate) fn changed_paths(&self) -> BTreeSet<BoardPath> {
        let mut paths = self.changed.clone();
        for path in &self.changed {
            if let BoardPath::Brief { name, .. } = path {
                paths.extend(
                    self.briefs
                        .iter()
                        .filter(|file| file.name == *name)
                        .map(|file| file.path(&self.name)),
                );
            }
        }
        paths
    }

    /// The bytes a board path of this project has now; `None` for a file the
    /// project does not have.
    pub(crate) fn bytes(&self, path: &BoardPath) -> Option<Vec<u8>> {
        match path {
            BoardPath::Item { project, id } if *project == self.name => {
                self.list(id.kind()).get(id.get()).map(Item::to_bytes)
            }
            BoardPath::Brief {
                project,
                name,
                place,
            } if *project == self.name => self
                .briefs
                .iter()
                .find(|file| file.name == *name && file.place == *place)
                .map(|file| file.brief.to_bytes()),
            BoardPath::Item { .. } | BoardPath::Brief { .. } => None,
        }
    }

    /// Reads the project from the files of the board-data clone at `root`.
    ///
    /// The files only: no git, no lock, no network. A read then shows what
    /// an editor shows, hand edits included, and cannot hang behind a write
    /// that waits for the network.
    pub fn load(root: &Path, name: &ProjectName) -> Result<Project, Error> {
        let mut project = Project::empty(name.clone());
        let mut files = Vec::new();
        for dir in DIRS {
            files_below(root, &Path::new(name.as_str()).join(dir), &mut files)?;
        }
        for kind in ListKind::ALL {
            let path = format!("{name}/{}", kind.file_name());
            let file = root.join(&path);
            match fs::read(&file) {
                Ok(bytes) => project.old_lists.push(OldList::new(kind, path, bytes)),
                Err(source) if source.kind() == io::ErrorKind::NotFound => {}
                Err(source) => return Err(Error::io("read", &file, source)),
            }
        }
        for relative in files {
            // The one path grammar decides what is a brief or an item; this
            // function only knows where they are looked for. A path that is
            // not UTF-8 is outside the grammar.
            match relative.to_str().and_then(BoardPath::parse) {
                Some(path) => {
                    let file = root.join(&relative);
                    let bytes =
                        fs::read(&file).map_err(|source| Error::io("read", &file, source))?;
                    project.add(path, &bytes);
                }
                None => {
                    let relative = relative.to_string_lossy();
                    match copy_of(&relative) {
                        Some(of) => project.copies.push((relative.into_owned(), of)),
                        None => project.not_the_boards(&relative),
                    }
                }
            }
        }
        Ok(project.sorted())
    }

    /// Reads the project from a commit of the board-data clone at `root`:
    /// what a write starts from, so that it applies to what the remote has
    /// and never to a file half saved in an editor.
    pub(crate) fn load_commit(
        root: &Path,
        name: &ProjectName,
        commit: &Oid,
    ) -> Result<Project, Error> {
        let git = Git::at(root);
        let unreadable = |failure: git::Failure| {
            Error::Git(format!("cannot read {name} at {commit}: {failure}"))
        };
        let mut project = Project::empty(name.clone());
        let dirs = DIRS.map(|dir| format!("{name}/{dir}/"));
        let mut wanted = Vec::new();
        let mut ids = Vec::new();
        let (mut old_lists, mut old_ids) = (Vec::new(), Vec::new());
        for (id, path) in git.files(commit, name.as_str()).map_err(unreadable)? {
            match BoardPath::parse(&path) {
                Some(board_path) => {
                    wanted.push(board_path);
                    ids.push(id);
                }
                None if dirs.iter().any(|dir| path.starts_with(dir.as_str())) => {
                    project.not_the_boards(&path);
                }
                None if let Some(kind) = ListKind::ALL
                    .into_iter()
                    .find(|kind| path == format!("{name}/{}", kind.file_name())) =>
                {
                    old_lists.push((kind, path));
                    old_ids.push(id);
                }
                None => {}
            }
        }
        ids.extend(old_ids);
        let mut blobs = git.blobs(&ids).map_err(unreadable)?;
        let old_blobs = blobs.split_off(wanted.len());
        for (path, bytes) in wanted.into_iter().zip(blobs) {
            project.add(path, &bytes);
        }
        for ((kind, path), bytes) in old_lists.into_iter().zip(old_blobs) {
            project.old_lists.push(OldList::new(kind, path, bytes));
        }
        Ok(project.sorted())
    }

    fn add(&mut self, path: BoardPath, bytes: &[u8]) {
        match path {
            BoardPath::Item { id, .. } => {
                self.list_mut(id.kind())
                    .insert(id.get(), Item::parse(bytes));
            }
            BoardPath::Brief { name, place, .. } => self.briefs.push(BriefFile {
                name,
                place,
                brief: Brief::parse(bytes),
            }),
        }
    }

    /// A file below `briefs/`, `chores/` or `backlog/` that the one path
    /// grammar does not take as the board's: lint names it when it looks
    /// like one, an `.md`.
    fn not_the_boards(&mut self, relative: &str) {
        if relative.ends_with(".md") {
            self.misnamed.push(relative.to_owned());
        }
    }

    fn sorted(mut self) -> Project {
        self.briefs.sort_by(brief_order);
        self.misnamed.sort();
        self.copies.sort();
        self
    }
}

/// By number, an open brief before a parked one before a filed one, then
/// by file name.
fn brief_order(a: &BriefFile, b: &BriefFile) -> std::cmp::Ordering {
    (a.name.number(), a.place, &a.name).cmp(&(b.name.number(), b.place, &b.name))
}

/// The paths a merge saves the local version of a brief at: `<path>.local`,
/// then `.local2`, `.local3`, … for one that is there already with other
/// bytes.
pub(crate) fn copy_paths(brief: &str) -> impl Iterator<Item = String> + '_ {
    (1u32..).map(move |n| match n {
        1 => format!("{brief}.local"),
        n => format!("{brief}.local{n}"),
    })
}

/// What a conflict copy is of, when `path` is one: a brief's or an item's
/// path with `.local` or `.local<n>` (n from 2) after it.
fn copy_of(path: &str) -> Option<ItemId> {
    let (of, n) = path.rsplit_once(".local")?;
    let numbered =
        n.is_empty() || (n.bytes().all(|b| b.is_ascii_digit()) && !n.starts_with('0') && n != "1");
    if !numbered {
        return None;
    }
    BoardPath::parse(of).map(|path| path.item_id())
}

/// The directories of a project the tool reads: what is below them is the
/// board's when the path grammar says so, else misnamed.
const DIRS: [&str; 3] = ["briefs", "chores", "backlog"];

/// Collects every file below `root/dir`, as paths relative to `root`. A
/// directory that is not there has no files.
fn files_below(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), Error> {
    let absolute = root.join(dir);
    let entries = match fs::read_dir(&absolute) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(Error::io("list", &absolute, source)),
    };
    for entry in entries {
        let entry = entry.map_err(|source| Error::io("list", &absolute, source))?;
        let relative = dir.join(entry.file_name());
        let kind = entry
            .file_type()
            .map_err(|source| Error::io("read", &entry.path(), source))?;
        if kind.is_dir() {
            files_below(root, &relative, files)?;
        } else {
            files.push(relative);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::ChoreId;
    use proptest::prelude::*;

    fn brief(path: &str) -> Option<(String, u32, String, bool)> {
        match BoardPath::parse(path)? {
            BoardPath::Brief {
                project,
                name,
                place,
            } => Some((
                project.to_string(),
                name.number().get(),
                name.slug().to_owned(),
                place == Place::Done,
            )),
            BoardPath::Item { .. } => None,
        }
    }

    #[test]
    fn the_four_shapes() {
        let shop = ProjectName::parse("shop").unwrap();
        assert_eq!(
            BoardPath::parse("shop/chores/c12.md"),
            Some(BoardPath::Item {
                project: shop.clone(),
                id: "c12".parse().unwrap()
            })
        );
        assert_eq!(
            BoardPath::parse("shop/backlog/b41.md"),
            Some(BoardPath::Item {
                project: shop,
                id: "b41".parse().unwrap()
            })
        );
        assert_eq!(
            brief("tools-shop/briefs/128-some-slug.md"),
            Some(("tools-shop".into(), 128, "some-slug".into(), false))
        );
        assert_eq!(
            brief("shop/briefs/done/007-Old Notes.v2.md"),
            Some(("shop".into(), 7, "Old Notes.v2".into(), true))
        );
    }

    #[test]
    fn not_the_boards() {
        for path in [
            "",
            "chores.md",
            "README.md",
            "shop",
            "shop/",
            "shop/README.md",
            "shop/chores.md/",
            "shop/Chores.md",
            "shop//chores.md",
            "./shop/chores.md",
            "/shop/chores.md",
            ".git/chores.md",
            "-shop/chores.md",
            "sh op/chores.md",
            "shop/sub/chores.md",
            "shop/done/1-a.md",
            "shop/briefs/notes.md",
            "shop/briefs/128.md",
            "shop/briefs/128-.md",
            "shop/briefs/128-a",
            "shop/briefs/128-a.md.local",
            "shop/briefs/-a.md",
            "shop/briefs/a-128.md",
            "shop/briefs/+128-a.md",
            "shop/briefs/4294967296-a.md",
            "shop/briefs/sub/128-a.md",
            "shop/briefs/done/done/128-a.md",
            "shop/briefs/done/128-a/b.md",
            // The list files are not the board's any more.
            "shop/chores.md",
            "shop/backlog.md",
            "shop/chores/b12.md",
            "shop/backlog/c12.md",
            "shop/chores/c012.md",
            "shop/chores/c0.md",
            "shop/chores/c12",
            "shop/chores/c12-slug.md",
            "shop/chores/sub/c12.md",
            "shop/backlog/b41.md.local",
            "shop/chores/notes.md",
        ] {
            assert_eq!(BoardPath::parse(path), None, "{path}");
        }
    }

    #[test]
    fn a_conflict_copy_is_a_brief_path_with_a_local_suffix() {
        let brief = "shop/briefs/done/128-x.md";
        let named: Vec<String> = copy_paths(brief).take(3).collect();
        assert_eq!(
            named,
            [
                "shop/briefs/done/128-x.md.local",
                "shop/briefs/done/128-x.md.local2",
                "shop/briefs/done/128-x.md.local3"
            ]
        );
        for path in &named {
            assert_eq!(
                copy_of(path),
                Some(ItemId::Brief(BriefId::new(128))),
                "{path}"
            );
        }
        assert_eq!(
            copy_of("shop/chores/c3.md.local2"),
            Some(ItemId::Chore(ChoreId::new(3).unwrap()))
        );
        for path in [
            "shop/briefs/done/128-x.md.local1",
            "shop/briefs/done/128-x.md.local02",
            "shop/briefs/done/128-x.md.localx",
            "shop/briefs/notes.md.local",
            "shop/chores.md.local",
            "shop/briefs/128-x.md",
        ] {
            assert_eq!(copy_of(path), None, "{path}");
        }
    }

    #[test]
    fn a_brief_named_done_dash_something_is_open() {
        // "done-…" is no number, so `briefs/done/` can never be read as a brief.
        assert_eq!(brief("shop/briefs/done-1-a.md"), None);
        assert_eq!(
            brief("shop/briefs/1-done.md"),
            Some(("shop".into(), 1, "done".into(), false))
        );
    }

    #[test]
    fn a_brief_in_done_moves_from_filed_whatever_its_header_says() {
        let now: Stamp = "2026-11-20T16:40:12Z".parse().unwrap();
        let file = |path: &str| match BoardPath::parse(path) {
            Some(BoardPath::Brief { name, place, .. }) => BriefFile {
                name,
                place,
                brief: Brief::parse(b"status: draft\n"),
            },
            _ => panic!("{path} is no brief path"),
        };
        let mut filed = file("shop/briefs/done/1-a.md");
        assert_eq!(
            filed.transition(Transition::Queue, now),
            Err(TransitionError::NotFrom(Status::Filed))
        );
        assert_eq!(filed.brief.to_bytes(), b"status: draft\n");
        let mut open = file("shop/briefs/1-a.md");
        assert_eq!(open.transition(Transition::Queue, now), Ok(Status::Queued));
    }

    #[test]
    fn slug_grammar() {
        for slug in [
            "a",
            "fix-login",
            "v2",
            "2026-11-review",
            &"a".repeat(Slug::MAX),
        ] {
            assert_eq!(
                slug.parse::<Slug>().map(|s| s.to_string()),
                Ok(slug.to_owned())
            );
        }
        for not_a_slug in [
            "",
            "-",
            "Fix-login",
            "fix--login",
            "-fix",
            "fix-",
            "fix_login",
            "fix login",
            "fix/login",
            "fix.md",
            "é",
            &"a".repeat(Slug::MAX + 1),
        ] {
            assert!(not_a_slug.parse::<Slug>().is_err(), "{not_a_slug}");
        }
        assert_eq!(
            "Fix".parse::<Slug>().unwrap_err().to_string(),
            "`Fix` is not a slug: words of a-z and 0-9 joined by single dashes, like fix-login, \
             at most 100 characters"
        );
    }

    proptest! {
        /// A name `new brief` gives is a brief path of the one grammar, with
        /// its number and slug.
        #[test]
        fn a_new_brief_s_name_is_a_brief_path(
            number in any::<u32>(),
            slug in "[a-z0-9]{1,5}(-[a-z0-9]{1,5}){0,3}",
        ) {
            let slug: Slug = slug.parse().unwrap();
            let name = BriefName::new(BriefId::new(number), &slug);
            prop_assert_eq!(name.number(), BriefId::new(number));
            prop_assert_eq!(name.slug(), slug.to_string());
            prop_assert_eq!(BriefName::parse(name.as_str()), Some(name.clone()));
            let path = format!("shop/briefs/{}", name.as_str());
            prop_assert_eq!(BoardPath::parse(&path).map(|p| p.to_string()), Some(path));
        }
    }

    fn any_board_path() -> impl Strategy<Value = String> {
        let project = "[A-Za-z0-9][A-Za-z0-9._-]{0,8}";
        let brief = "[0-9]{1,9}-[^/]{1,12}\\.md";
        prop_oneof![
            (project, "(chores/c|backlog/b)[1-9][0-9]{0,8}\\.md")
                .prop_map(|(p, f)| format!("{p}/{f}")),
            (project, "(done/|parked/)?", brief).prop_map(|(p, d, b)| format!("{p}/briefs/{d}{b}")),
        ]
    }

    /// Near the grammar on purpose, about half of these parse: an extra
    /// `done/`, a slash in the name, a missing number or `.md`.
    fn almost_a_board_path() -> impl Strategy<Value = String> {
        prop_oneof![
            "[a-z0-9.]{1,3}/(chores|backlog|notes)(/[bc]0?[0-9]{0,2})?\\.md/?",
            "[a-z0-9.]{1,3}/briefs/((done|parked)/){0,3}[0-9]{0,3}-?[a-z/]{0,2}\\.md",
            "([a-z.]{0,3}/){0,2}(chores\\.md|briefs/|done/|[0-9]{0,2}-[a-z]{0,2}(\\.md)?){0,3}",
        ]
    }

    fn below(toplevel: &str) -> Result<String, NoName> {
        name_below(Path::new("/home/u/Work"), Path::new(toplevel)).map(|name| name.to_string())
    }

    #[test]
    fn a_project_is_named_by_its_path_below_work() {
        assert_eq!(below("/home/u/Work/shop"), Ok("shop".to_owned()));
        assert_eq!(
            below("/home/u/Work/tools/shop"),
            Ok("tools-shop".to_owned())
        );
        assert_eq!(below("/home/u/Work/a/b.c/d_e"), Ok("a-b.c-d_e".to_owned()));
        for outside in [
            "/home/u/Work",
            "/home/u",
            "/home/u/Workshop/shop",
            "/home/u/elsewhere/shop",
            "/",
        ] {
            assert_eq!(below(outside), Err(NoName::Outside), "{outside}");
        }
        assert_eq!(
            below("/home/u/Work/my shop"),
            Err(NoName::Not("my shop".to_owned()))
        );
        assert_eq!(
            below("/home/u/Work/.hidden/shop"),
            Err(NoName::Not(".hidden-shop".to_owned()))
        );
    }

    proptest! {
        /// The derived name is a project name whenever every directory on
        /// the way is one, so a project nested below `Work` at any depth has
        /// a board.
        #[test]
        fn directories_that_are_project_names_give_a_project_name(
            parts in prop::collection::vec("[A-Za-z0-9][A-Za-z0-9._-]{0,6}", 1..4),
        ) {
            let toplevel = parts.iter().fold(PathBuf::from("/home/u/Work"), |path, part| path.join(part));
            prop_assert_eq!(
                name_below(Path::new("/home/u/Work"), &toplevel).map(|name| name.to_string()),
                Ok(parts.join("-"))
            );
        }

        #[test]
        fn every_path_of_the_grammar_parses_and_prints_itself(path in any_board_path()) {
            let parsed = BoardPath::parse(&path);
            prop_assert_eq!(parsed.map(|p| p.to_string()), Some(path));
        }

        /// One spelling per path: what parses prints back as it was read, so
        /// two strings never name the same board file.
        #[test]
        fn a_path_that_parses_prints_itself(path in almost_a_board_path()) {
            if let Some(parsed) = BoardPath::parse(&path) {
                prop_assert_eq!(parsed.to_string(), &path[..]);
                // Each part is one path segment.
                let segments = match parsed {
                    BoardPath::Item { .. } => 3,
                    BoardPath::Brief { place: Place::Open, .. } => 3,
                    BoardPath::Brief { place: Place::Parked | Place::Done, .. } => 4,
                };
                prop_assert_eq!(path.split('/').count(), segments);
            }
        }
    }
}
