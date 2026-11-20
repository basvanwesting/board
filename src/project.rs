//! Which project a command is about, where its files sit in the board-data
//! repo, and the files as read.

use std::collections::BTreeSet;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::brief::{BadStatus, Brief, Status};
use crate::error::Error;
use crate::git::{self, Git, Oid};
use crate::id::{BriefId, ListKind};
use crate::list::ListFile;

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
    const HINT: &str = "name the board with --board <name>";
    if let Some(name) = &env.board {
        return ProjectName::parse(name)
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
    if checkout == root {
        return Err(Error::Usage(format!(
            "no project for {shown}: it is the board-data clone, which holds every project's \
             board; {HINT}"
        )));
    }
    name_below(&work, &checkout).map_err(|why| {
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

/// A path the board owns, relative to `$BOARD_ROOT`:
///
/// ```text
/// <project>/chores.md | <project>/backlog.md
/// <project>/briefs/<brief> | <project>/briefs/done/<brief>
/// ```
///
/// Anything else in the repo is not the board's: never committed by the tool,
/// never merged.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BoardPath {
    List {
        project: ProjectName,
        kind: ListKind,
    },
    Brief {
        project: ProjectName,
        name: BriefName,
        /// Under `briefs/done/`.
        filed: bool,
    },
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
        if let Some(kind) = ListKind::ALL.into_iter().find(|k| k.file_name() == rest) {
            return Some(BoardPath::List { project, kind });
        }
        let brief = rest.strip_prefix("briefs/")?;
        let (filed, file_name) = match brief.strip_prefix("done/") {
            Some(file_name) => (true, file_name),
            None => (false, brief),
        };
        let name = BriefName::parse(file_name)?;
        Some(BoardPath::Brief {
            project,
            name,
            filed,
        })
    }

    pub fn project(&self) -> &ProjectName {
        match self {
            BoardPath::List { project, .. } | BoardPath::Brief { project, .. } => project,
        }
    }
}

impl fmt::Display for BoardPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BoardPath::List { project, kind } => write!(f, "{project}/{}", kind.file_name()),
            BoardPath::Brief {
                project,
                name,
                filed: false,
            } => write!(f, "{project}/briefs/{}", name.as_str()),
            BoardPath::Brief {
                project,
                name,
                filed: true,
            } => write!(f, "{project}/briefs/done/{}", name.as_str()),
        }
    }
}

/// One brief file of a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BriefFile {
    pub name: BriefName,
    /// Under `briefs/done/`.
    pub filed: bool,
    pub brief: Brief,
}

impl BriefFile {
    /// The brief's status on the board.
    ///
    /// Where the file sits decides whether a brief is filed, not its header:
    /// an archived brief placed in `briefs/done/` by hand has no header, and
    /// a file has one place where a header can say anything. So `filed` in
    /// the header of a brief outside `done/` is no status either: nothing
    /// moves on from it, and the board would show a filed brief among the
    /// open ones.
    pub fn status(&self) -> Result<Status, BadStatus> {
        if self.filed {
            return Ok(Status::Filed);
        }
        match self.brief.status()? {
            Status::Filed => Err(BadStatus::Unknown(Status::Filed.as_str().to_owned())),
            status @ (Status::Draft
            | Status::Queued
            | Status::Started
            | Status::Blocked
            | Status::Reported) => Ok(status),
        }
    }

    pub fn path(&self, project: &ProjectName) -> BoardPath {
        BoardPath::Brief {
            project: project.clone(),
            name: self.name.clone(),
            filed: self.filed,
        }
    }
}

/// A project's board, as its files hold it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub(crate) name: ProjectName,
    pub(crate) chores: ListFile,
    pub(crate) backlog: ListFile,
    /// By number, an open brief before a filed one, then by file name.
    pub(crate) briefs: Vec<BriefFile>,
    /// The `.md` files below `briefs/` that are outside the path grammar, as
    /// paths relative to the board-data root. The tool does not sync them;
    /// lint says so.
    pub(crate) misnamed: Vec<String>,
    /// The files an edit went through since the load: what a write commits.
    pub(crate) changed: BTreeSet<BoardPath>,
}

impl Project {
    /// A project without files, which is also what a project without a
    /// directory is: its board is empty, not missing.
    pub fn empty(name: ProjectName) -> Project {
        Project {
            name,
            chores: ListFile::parse(ListKind::Chores, b""),
            backlog: ListFile::parse(ListKind::Backlog, b""),
            briefs: Vec::new(),
            misnamed: Vec::new(),
            changed: BTreeSet::new(),
        }
    }

    pub fn name(&self) -> &ProjectName {
        &self.name
    }

    pub fn list(&self, kind: ListKind) -> &ListFile {
        match kind {
            ListKind::Chores => &self.chores,
            ListKind::Backlog => &self.backlog,
        }
    }

    pub fn briefs(&self) -> &[BriefFile] {
        &self.briefs
    }

    /// A list to edit; the edit is committed by the write it runs in.
    pub(crate) fn list_mut(&mut self, kind: ListKind) -> &mut ListFile {
        self.changed.insert(BoardPath::List {
            project: self.name.clone(),
            kind,
        });
        match kind {
            ListKind::Chores => &mut self.chores,
            ListKind::Backlog => &mut self.backlog,
        }
    }

    /// The bytes a board path of this project has now; `None` for a file the
    /// project does not have.
    pub(crate) fn bytes(&self, path: &BoardPath) -> Option<Vec<u8>> {
        match path {
            BoardPath::List { project, kind } if *project == self.name => {
                Some(self.list(*kind).to_bytes())
            }
            BoardPath::Brief {
                project,
                name,
                filed,
            } if *project == self.name => self
                .briefs
                .iter()
                .find(|file| file.name == *name && file.filed == *filed)
                .map(|file| file.brief.to_bytes()),
            BoardPath::List { .. } | BoardPath::Brief { .. } => None,
        }
    }

    /// Reads the project from the files of the board-data clone at `root`.
    ///
    /// The files only: no git, no lock, no network. A read then shows what
    /// an editor shows, hand edits included, and cannot hang behind a write
    /// that waits for the network.
    pub fn load(root: &Path, name: &ProjectName) -> Result<Project, Error> {
        let dir = root.join(name.as_str());
        let mut project = Project::empty(name.clone());
        for kind in ListKind::ALL {
            if let Some(bytes) = read_if_there(&dir.join(kind.file_name()))? {
                project.set_list(kind, &bytes);
            }
        }
        let mut files = Vec::new();
        files_below(root, &Path::new(name.as_str()).join("briefs"), &mut files)?;
        for relative in files {
            // The one path grammar decides what is a brief; this function
            // only knows where briefs are looked for. A path that is not
            // UTF-8 is outside the grammar.
            match relative.to_str().and_then(BoardPath::parse) {
                Some(BoardPath::Brief {
                    name: brief_name,
                    filed,
                    ..
                }) => {
                    let file = root.join(&relative);
                    let bytes =
                        fs::read(&file).map_err(|source| Error::io("read", &file, source))?;
                    project.add_brief(brief_name, filed, &bytes);
                }
                Some(BoardPath::List { .. }) | None => {
                    project.not_a_brief(&relative.to_string_lossy());
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
        let briefs_dir = format!("{name}/briefs/");
        let mut wanted = Vec::new();
        let mut ids = Vec::new();
        for (id, path) in git.files(commit, name.as_str()).map_err(unreadable)? {
            match BoardPath::parse(&path) {
                Some(board_path) => {
                    wanted.push(board_path);
                    ids.push(id);
                }
                None if path.starts_with(&briefs_dir) => project.not_a_brief(&path),
                None => {}
            }
        }
        let blobs = git.blobs(&ids).map_err(unreadable)?;
        for (path, bytes) in wanted.into_iter().zip(blobs) {
            match path {
                BoardPath::List { kind, .. } => project.set_list(kind, &bytes),
                BoardPath::Brief {
                    name: brief_name,
                    filed,
                    ..
                } => project.add_brief(brief_name, filed, &bytes),
            }
        }
        Ok(project.sorted())
    }

    fn set_list(&mut self, kind: ListKind, bytes: &[u8]) {
        let file = ListFile::parse(kind, bytes);
        match kind {
            ListKind::Chores => self.chores = file,
            ListKind::Backlog => self.backlog = file,
        }
    }

    fn add_brief(&mut self, name: BriefName, filed: bool, bytes: &[u8]) {
        self.briefs.push(BriefFile {
            name,
            filed,
            brief: Brief::parse(bytes),
        });
    }

    /// A file below `briefs/` that the one path grammar does not take as a
    /// brief: lint names it when it looks like one, an `.md`.
    fn not_a_brief(&mut self, relative: &str) {
        if relative.ends_with(".md") {
            self.misnamed.push(relative.to_owned());
        }
    }

    fn sorted(mut self) -> Project {
        self.briefs.sort_by(|a, b| {
            (a.name.number(), a.filed, &a.name).cmp(&(b.name.number(), b.filed, &b.name))
        });
        self.misnamed.sort();
        self
    }
}

fn read_if_there(file: &Path) -> Result<Option<Vec<u8>>, Error> {
    match fs::read(file) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::io("read", file, source)),
    }
}

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
    use proptest::prelude::*;

    fn brief(path: &str) -> Option<(String, u32, String, bool)> {
        match BoardPath::parse(path)? {
            BoardPath::Brief {
                project,
                name,
                filed,
            } => Some((
                project.to_string(),
                name.number().get(),
                name.slug().to_owned(),
                filed,
            )),
            BoardPath::List { .. } => None,
        }
    }

    #[test]
    fn the_four_shapes() {
        let shop = ProjectName::parse("shop").unwrap();
        assert_eq!(
            BoardPath::parse("shop/chores.md"),
            Some(BoardPath::List {
                project: shop.clone(),
                kind: ListKind::Chores
            })
        );
        assert_eq!(
            BoardPath::parse("shop/backlog.md"),
            Some(BoardPath::List {
                project: shop,
                kind: ListKind::Backlog
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
        ] {
            assert_eq!(BoardPath::parse(path), None, "{path}");
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

    fn any_board_path() -> impl Strategy<Value = String> {
        let project = "[A-Za-z0-9][A-Za-z0-9._-]{0,8}";
        let brief = "[0-9]{1,9}-[^/]{1,12}\\.md";
        prop_oneof![
            (project, "(chores|backlog)\\.md").prop_map(|(p, f)| format!("{p}/{f}")),
            (project, "(done/)?", brief).prop_map(|(p, d, b)| format!("{p}/briefs/{d}{b}")),
        ]
    }

    /// Near the grammar on purpose, about half of these parse: an extra
    /// `done/`, a slash in the name, a missing number or `.md`.
    fn almost_a_board_path() -> impl Strategy<Value = String> {
        prop_oneof![
            "[a-z0-9.]{1,3}/(chores|backlog|notes)\\.md/?",
            "[a-z0-9.]{1,3}/briefs/(done/){0,3}[0-9]{0,3}-?[a-z/]{0,2}\\.md",
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
                    BoardPath::List { .. } => 2,
                    BoardPath::Brief { filed: false, .. } => 3,
                    BoardPath::Brief { filed: true, .. } => 4,
                };
                prop_assert_eq!(path.split('/').count(), segments);
            }
        }
    }
}
