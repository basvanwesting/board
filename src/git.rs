//! The one place that spawns `git` (`clippy.toml` refuses `Command::new`
//! everywhere else), so every call gets the same clean surroundings.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;

/// What `git rev-parse --local-env-vars` lists (git 2.56): the variables that
/// tie a git process to one repository.
///
/// Every call removes them. `board` is also started from inside git, by a
/// hook or `git rebase --exec` in some project, and git sets `GIT_DIR` and
/// `GIT_INDEX_FILE` for what it starts: inherited, they would point this
/// tool's git at that repository instead of the one named with `-C`.
///
/// Hard-coded, because asking git costs a process per command; a unit test
/// compares the list with the installed git's.
const LOCAL_ENV_VARS: [&str; 15] = [
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

/// Configuration no user setting may change under the tool: its output is
/// parsed and its files are bytes.
const CONFIG: [&str; 3] = [
    "core.autocrlf=false",
    "commit.gpgsign=false",
    "color.ui=false",
];

/// For fetch and push: a connection that stalls is given up instead of
/// holding the board's lock for as long as TCP waits.
const NETWORK: [&str; 2] = ["http.lowSpeedLimit=1000", "http.lowSpeedTime=20"];

/// Why a git call gave no result. The two are kept apart because callers
/// treat them differently: a git that said no is often an answer (not a
/// repository), a git that did not run never is.
#[derive(Debug)]
pub(crate) enum Failure {
    /// git could not be started.
    NotRun(io::Error),
    /// git ran and failed; the first line of what it said.
    Said(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::NotRun(source) => write!(f, "cannot run git: {source}"),
            Failure::Said(said) => f.write_str(said),
        }
    }
}

/// A commit, tree or blob, as git prints its id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Oid(String);

impl Oid {
    fn parse(stdout: &[u8]) -> Result<Oid, Failure> {
        let text = stdout.strip_suffix(b"\n").unwrap_or(stdout);
        match str::from_utf8(text) {
            Ok(hex)
                if matches!(hex.len(), 40 | 64)
                    && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) =>
            {
                Ok(Oid(hex.to_owned()))
            }
            _ => Err(Failure::Said(format!(
                "git gave `{}` where an object id was expected",
                String::from_utf8_lossy(text).escape_debug()
            ))),
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a tree holds at a path: its mode (`100644`, `120000`, …) and object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TreeEntry {
    pub(crate) mode: String,
    pub(crate) id: Oid,
}

impl TreeEntry {
    /// A regular file with these contents.
    pub(crate) fn file(id: Oid) -> TreeEntry {
        TreeEntry {
            mode: "100644".to_owned(),
            id,
        }
    }
}

/// What a call needs beyond its arguments.
#[derive(Default, Clone, Copy)]
struct Call<'a> {
    /// Written to git's stdin; without it stdin is empty.
    input: Option<&'a [u8]>,
    /// `GIT_INDEX_FILE`: an index of the tool's own, not the clone's.
    index: Option<&'a Path>,
    /// A fetch or a push.
    network: bool,
}

/// git, run in one directory.
pub(crate) struct Git {
    dir: PathBuf,
}

impl Git {
    pub(crate) fn at(dir: &Path) -> Git {
        Git {
            dir: dir.to_owned(),
        }
    }

    /// Starts `git <args>` in the directory and waits for it.
    fn spawn(&self, args: &[&str], call: Call) -> Result<Output, Failure> {
        #[expect(
            clippy::disallowed_methods,
            reason = "this is the one place that spawns git"
        )]
        let mut command = Command::new("git");
        for variable in LOCAL_ENV_VARS {
            command.env_remove(variable);
        }
        command
            // No credential or passphrase question on a terminal: the tool
            // also runs where nobody answers, and it may hold a lock then.
            .env("GIT_TERMINAL_PROMPT", "0")
            // Messages are quoted to the user and, in one place, read.
            .env("LC_ALL", "C")
            // A path is a path: a project or brief name is never a glob or
            // a pathspec magic word.
            .env("GIT_LITERAL_PATHSPECS", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // What the command reads is not git's to consume.
            .stdin(if call.input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        if let Some(index) = call.index {
            command.env("GIT_INDEX_FILE", index);
        }
        for setting in CONFIG {
            command.args(["-c", setting]);
        }
        if call.network {
            for setting in NETWORK {
                command.args(["-c", setting]);
            }
        }
        command.arg("-C").arg(&self.dir).args(args);
        let mut child = command.spawn().map_err(Failure::NotRun)?;
        let Some(input) = call.input else {
            return child.wait_with_output().map_err(Failure::NotRun);
        };
        let mut stdin = child.stdin.take().expect("stdin was piped");
        // Written beside the read, so neither side waits on a full pipe.
        thread::scope(|scope| {
            let writer = scope.spawn(move || {
                // A git that stops reading has failed and says why on
                // stderr; that is the error worth reporting.
                let _ = stdin.write_all(input);
            });
            let output = child.wait_with_output().map_err(Failure::NotRun);
            let _ = writer.join();
            output
        })
    }

    /// Runs `git <args>` and gives its stdout.
    fn run(&self, args: &[&str]) -> Result<Vec<u8>, Failure> {
        self.run_with(args, Call::default())
    }

    fn run_with(&self, args: &[&str], call: Call) -> Result<Vec<u8>, Failure> {
        let output = self.spawn(args, call)?;
        succeeded(args, output)
    }

    /// Runs a `git <args>` that answers a yes/no question with its exit
    /// code: `None` when git exits with 1, its "no".
    fn ask(&self, args: &[&str]) -> Result<Option<Vec<u8>>, Failure> {
        let output = self.spawn(args, Call::default())?;
        if output.status.code() == Some(1) {
            return Ok(None);
        }
        succeeded(args, output).map(Some)
    }

    /// Runs a `git <args>` that prints one path.
    fn path(&self, args: &[&str]) -> Result<PathBuf, Failure> {
        let stdout = self.run(args)?;
        // One line; a path may hold any other byte.
        let path = stdout.strip_suffix(b"\n").unwrap_or(&stdout);
        Ok(PathBuf::from(OsString::from_vec(path.to_vec())))
    }

    /// The top directory of the work tree the directory is in.
    pub(crate) fn toplevel(&self) -> Result<PathBuf, Failure> {
        self.path(&["rev-parse", "--show-toplevel"])
    }

    /// The git directory of the repository the directory's work tree was
    /// added to with `git worktree add`; `None` in a work tree that is its
    /// repository's own.
    ///
    /// An added work tree has a git directory of its own (its `HEAD` and
    /// index, in `<repository>/worktrees/<name>`) beside the one it shares
    /// with every other; the repository's own has only the one. That is the
    /// difference git tells: it has no question for "is this an added work
    /// tree", and `git worktree list` gives the git directory, not the work
    /// tree, as the first one of a submodule.
    pub(crate) fn added_to(&self) -> Result<Option<PathBuf>, Failure> {
        let own = self.path(&["rev-parse", "--path-format=absolute", "--git-dir"])?;
        let shared = self.path(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        Ok((own != shared).then_some(shared))
    }

    /// The ref `HEAD` points at; `None` for a detached `HEAD`.
    pub(crate) fn head_ref(&self) -> Result<Option<String>, Failure> {
        Ok(self
            .ask(&["symbolic-ref", "-q", "HEAD"])?
            .map(|stdout| String::from_utf8_lossy(&stdout).trim_end().to_owned()))
    }

    /// Whether the remote `origin` is configured.
    pub(crate) fn has_origin(&self) -> Result<bool, Failure> {
        Ok(self
            .ask(&["config", "--get", "remote.origin.url"])?
            .is_some())
    }

    /// The commit a ref names; `None` when there is none.
    pub(crate) fn commit(&self, name: &str) -> Result<Option<Oid>, Failure> {
        let name = format!("{name}^{{commit}}");
        self.ask(&["rev-parse", "-q", "--verify", "--end-of-options", &name])?
            .map(|stdout| Oid::parse(&stdout))
            .transpose()
    }

    /// Whether `ancestor` is `descendant` or one of its ancestors.
    pub(crate) fn is_ancestor(&self, ancestor: &Oid, descendant: &Oid) -> Result<bool, Failure> {
        Ok(self
            .ask(&[
                "merge-base",
                "--is-ancestor",
                ancestor.as_str(),
                descendant.as_str(),
            ])?
            .is_some())
    }

    /// `origin`'s `main` into `refs/remotes/origin/main`, forced: what the
    /// remote holds is the truth, whatever was fetched before.
    pub(crate) fn fetch(&self) -> Result<(), Failure> {
        self.run_with(
            &[
                "fetch",
                "-q",
                "--no-tags",
                "--no-recurse-submodules",
                "origin",
                "+refs/heads/main:refs/remotes/origin/main",
            ],
            Call {
                network: true,
                ..Call::default()
            },
        )
        .map(drop)
    }

    /// Pushes a commit to `origin`'s `main`, as a fast-forward only.
    pub(crate) fn push(&self, commit: &Oid) -> Result<(), Failure> {
        let refspec = format!("{commit}:refs/heads/main");
        self.run_with(
            &[
                "push",
                "-q",
                "--no-signed",
                "--no-follow-tags",
                "--no-recurse-submodules",
                "origin",
                &refspec,
            ],
            Call {
                network: true,
                ..Call::default()
            },
        )
        .map(drop)
    }

    /// Moves the checked-out branch to `commit`, which has it as an
    /// ancestor. git refuses rather than overwrite a file changed in the
    /// work tree.
    pub(crate) fn fast_forward(&self, commit: &Oid) -> Result<(), Failure> {
        self.run(&[
            "merge",
            "-q",
            "--ff-only",
            "--no-autostash",
            "--no-verify-signatures",
            commit.as_str(),
        ])
        .map(drop)
    }

    /// The paths that differ between `HEAD`, the index and the work tree,
    /// untracked files one by one, relative to the top. A path that is not
    /// UTF-8 is left out: no board path is.
    pub(crate) fn changed_paths(&self) -> Result<Vec<String>, Failure> {
        let stdout = self.run(&[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--no-renames",
            "--ignore-submodules=none",
        ])?;
        Ok(stdout
            .split(|b| *b == 0)
            .filter_map(|entry| entry.get(3..))
            .filter_map(|path| str::from_utf8(path).ok())
            .map(str::to_owned)
            .collect())
    }

    /// The blobs below `dir` in a commit's tree, as `(id, path)`, paths
    /// relative to the top. A symlink or a submodule holds no board file and
    /// is left out.
    pub(crate) fn files(&self, commit: &Oid, dir: &str) -> Result<Vec<(Oid, String)>, Failure> {
        Ok(self
            .ls_tree(commit, Some(&format!("{dir}/")))?
            .into_iter()
            .filter(|(_, entry)| matches!(entry.mode.as_str(), "100644" | "100755"))
            .map(|(path, entry)| (entry.id, path))
            .collect())
    }

    /// Every entry of a commit's tree but the directories: files, symlinks
    /// and submodules, by path relative to the top. A path that is not UTF-8
    /// is left out: no board path is, and the merge keeps the remote's
    /// entry for it.
    pub(crate) fn tree(&self, commit: &Oid) -> Result<BTreeMap<String, TreeEntry>, Failure> {
        Ok(self.ls_tree(commit, None)?.into_iter().collect())
    }

    fn ls_tree(
        &self,
        commit: &Oid,
        dir: Option<&str>,
    ) -> Result<Vec<(String, TreeEntry)>, Failure> {
        let mut args = vec!["ls-tree", "-r", "-z", "--full-tree", commit.as_str()];
        if let Some(dir) = dir {
            args.extend(["--", dir]);
        }
        let stdout = self.run(&args)?;
        let mut entries = Vec::new();
        for entry in stdout.split(|b| *b == 0).filter(|entry| !entry.is_empty()) {
            // `<mode> SP <type> SP <id> TAB <path>`
            let malformed = || Failure::Said("git ls-tree printed an unexpected line".to_owned());
            let tab = entry
                .iter()
                .position(|b| *b == b'\t')
                .ok_or_else(malformed)?;
            let mut fields = entry[..tab].split(|b| *b == b' ');
            let (Some(mode), Some(_kind), Some(id)) = (fields.next(), fields.next(), fields.next())
            else {
                return Err(malformed());
            };
            if let (Ok(path), Ok(mode)) = (str::from_utf8(&entry[tab + 1..]), str::from_utf8(mode))
            {
                entries.push((
                    path.to_owned(),
                    TreeEntry {
                        mode: mode.to_owned(),
                        id: Oid::parse(id)?,
                    },
                ));
            }
        }
        Ok(entries)
    }

    /// The contents of blobs, in the order asked, in one call.
    pub(crate) fn blobs(&self, ids: &[Oid]) -> Result<Vec<Vec<u8>>, Failure> {
        let names: Vec<String> = ids.iter().map(Oid::to_string).collect();
        self.cat_blobs(&names)?
            .into_iter()
            .zip(&names)
            .map(|(blob, name)| {
                blob.ok_or_else(|| Failure::Said(format!("git cat-file: {name} missing")))
            })
            .collect()
    }

    /// A file as each of `commits` has it, `None` where it has none, in one
    /// call.
    pub(crate) fn file_at(
        &self,
        commits: &[Oid],
        path: &str,
    ) -> Result<Vec<Option<Vec<u8>>>, Failure> {
        let names: Vec<String> = commits
            .iter()
            .map(|commit| format!("{commit}:{path}"))
            .collect();
        self.cat_blobs(&names)
    }

    /// The contents of the blobs `names` name (an id, `<commit>:<path>`), in
    /// the order asked; `None` for a name that names nothing.
    fn cat_blobs(&self, names: &[String]) -> Result<Vec<Option<Vec<u8>>>, Failure> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let input: String = names.iter().map(|name| format!("{name}\n")).collect();
        let stdout = self.run_with(
            &["cat-file", "--batch"],
            Call {
                input: Some(input.as_bytes()),
                ..Call::default()
            },
        )?;
        // Per name: `<id> SP blob SP <size> LF <contents> LF`, or
        // `<name> SP missing LF`.
        let malformed = || Failure::Said("git cat-file printed an unexpected answer".to_owned());
        let mut rest = &stdout[..];
        let mut blobs = Vec::with_capacity(names.len());
        for name in names {
            let end = rest
                .iter()
                .position(|b| *b == b'\n')
                .ok_or_else(malformed)?;
            let head = str::from_utf8(&rest[..end]).map_err(|_| malformed())?;
            if head.strip_suffix(" missing") == Some(name.as_str()) {
                blobs.push(None);
                rest = &rest[end + 1..];
                continue;
            }
            let size: usize = match head.split(' ').collect::<Vec<_>>()[..] {
                [_, "blob", size] => size.parse().map_err(|_| malformed())?,
                _ => return Err(Failure::Said(format!("git cat-file: {head}"))),
            };
            let contents = rest.get(end + 1..end + 1 + size).ok_or_else(malformed)?;
            blobs.push(Some(contents.to_vec()));
            rest = rest.get(end + 1 + size + 1..).ok_or_else(malformed)?;
        }
        Ok(blobs)
    }

    /// The commits that are descendants of `from` and ancestors of `to`
    /// (`to` included), parents before children.
    pub(crate) fn ancestry_path(&self, from: &Oid, to: &Oid) -> Result<Vec<Oid>, Failure> {
        let range = format!("{from}..{to}");
        let stdout = self.run(&[
            "rev-list",
            "--ancestry-path",
            "--topo-order",
            "--reverse",
            &range,
        ])?;
        stdout
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .map(Oid::parse)
            .collect()
    }

    /// The patches of every commit reachable from `commit` that touched
    /// `path`, as zero-context hunks, following the file across renames and
    /// through every side branch.
    ///
    /// `--follow` keeps the commits before a rename of the project's
    /// directory. Plain `git log -- <path>` simplifies history: an id added
    /// and removed again on a merged side branch is not in it. `--follow`
    /// turns that off too (git 2.56); `--full-history` says it on its own,
    /// so it does not hang on `--follow`.
    ///
    /// `--follow` alone misses the path's own older history when a rename
    /// recreates it: at that commit it goes on with the source path only. A
    /// project directory removed and its name used again by renaming another
    /// onto it would free the old file's ids. So the path's log without
    /// `--follow` is read too, and the two are given one after the other.
    ///
    /// The lines are parsed, so what the user's configuration or attributes
    /// could change in them is turned off: colour, a textconv driver, a
    /// signature check printed in the log, and a file taken for binary
    /// (`-diff`, `binary` or a NUL byte), whose patch would hold no lines.
    /// An external diff needs `--ext-diff` in `git log`, which no
    /// configuration sets.
    pub(crate) fn file_history(&self, commit: &Oid, path: &str) -> Result<Vec<u8>, Failure> {
        let log = |follow: &[&str]| {
            self.run(
                &[
                    &["log"],
                    follow,
                    &[
                        "--full-history",
                        "--format=",
                        "-p",
                        "-U0",
                        "--no-color",
                        "--text",
                        "--no-textconv",
                        "--no-show-signature",
                        commit.as_str(),
                        "--",
                        path,
                    ],
                ]
                .concat(),
            )
        };
        let mut patches = log(&["--follow"])?;
        patches.extend(log(&[])?);
        Ok(patches)
    }

    /// Stores a blob and gives its id. The bytes go in as they are: no
    /// line-ending or other filter applies.
    pub(crate) fn hash_object(&self, bytes: &[u8]) -> Result<Oid, Failure> {
        let stdout = self.run_with(
            &["hash-object", "-w", "--no-filters", "--stdin"],
            Call {
                input: Some(bytes),
                ..Call::default()
            },
        )?;
        Oid::parse(&stdout)
    }

    /// Fills the index file `index` with a commit's tree.
    pub(crate) fn read_tree(&self, index: &Path, commit: &Oid) -> Result<(), Failure> {
        self.run_with(
            &["read-tree", commit.as_str()],
            Call {
                index: Some(index),
                ..Call::default()
            },
        )
        .map(drop)
    }

    /// Sets paths of the index file `index` to entries; `None` removes the
    /// path.
    pub(crate) fn update_index(
        &self,
        index: &Path,
        files: &[(String, Option<TreeEntry>)],
    ) -> Result<(), Failure> {
        let mut set = Vec::new();
        let mut removed = Vec::new();
        for (path, entry) in files {
            match entry {
                Some(TreeEntry { mode, id }) => {
                    set.extend(format!("{mode} {id}\t{path}\0").into_bytes());
                }
                // Not as `--index-info` with mode 0: that line needs an
                // object id of the repository's hash length.
                None => removed.extend(format!("{path}\0").into_bytes()),
            }
        }
        for (args, input) in [
            (&["update-index", "-z", "--index-info"][..], set),
            (
                &["update-index", "-z", "--force-remove", "--stdin"][..],
                removed,
            ),
        ] {
            if !input.is_empty() {
                self.run_with(
                    args,
                    Call {
                        input: Some(&input),
                        index: Some(index),
                        ..Call::default()
                    },
                )?;
            }
        }
        Ok(())
    }

    /// Writes the index file `index` as a tree.
    pub(crate) fn write_tree(&self, index: &Path) -> Result<Oid, Failure> {
        let stdout = self.run_with(
            &["write-tree"],
            Call {
                index: Some(index),
                ..Call::default()
            },
        )?;
        Oid::parse(&stdout)
    }

    /// A commit of a tree on its parents, the first parent first; no ref
    /// moves.
    pub(crate) fn commit_tree(
        &self,
        tree: &Oid,
        parents: &[&Oid],
        message: &str,
    ) -> Result<Oid, Failure> {
        let mut args = vec!["commit-tree", tree.as_str()];
        for parent in parents {
            args.extend(["-p", parent.as_str()]);
        }
        args.extend(["-m", message]);
        let stdout = self.run(&args)?;
        Oid::parse(&stdout)
    }

    /// The best common ancestor of two commits.
    pub(crate) fn merge_base(&self, a: &Oid, b: &Oid) -> Result<Oid, Failure> {
        let stdout = self.run(&["merge-base", a.as_str(), b.as_str()])?;
        Oid::parse(&stdout)
    }

    /// How many commits `to` has that `from` has not.
    pub(crate) fn count_between(&self, from: &Oid, to: &Oid) -> Result<usize, Failure> {
        let range = format!("{from}..{to}");
        let stdout = self.run(&["rev-list", "--count", &range])?;
        String::from_utf8_lossy(&stdout)
            .trim()
            .parse()
            .map_err(|_| Failure::Said("git rev-list --count printed no number".to_owned()))
    }

    /// The three blobs' lines merged, with both sides of a conflict kept,
    /// ours first: the line order of a list merge.
    pub(crate) fn union(&self, ours: &Oid, base: &Oid, theirs: &Oid) -> Result<Vec<u8>, Failure> {
        // With `--union` there are no conflicts, so a non-zero exit is an
        // error.
        self.run(&[
            "merge-file",
            "-p",
            "--union",
            "--object-id",
            ours.as_str(),
            base.as_str(),
            theirs.as_str(),
        ])
    }

    /// Commits `paths` as the work tree has them, added, changed or deleted,
    /// and nothing else: whatever else is staged stays staged. No hook runs
    /// that could refuse or change the commit.
    pub(crate) fn commit_paths(&self, paths: &[String], message: &str) -> Result<(), Failure> {
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        self.run(&[&["add", "-A", "--"][..], &paths].concat())?;
        // Only the paths that now differ from HEAD. A file staged as new and
        // then deleted (a write killed between its add and its commit, then
        // the file removed by hand) is in neither HEAD nor the index after
        // the add, and `commit -- <path>` refuses the whole commit for a path
        // git does not know.
        let staged = self.run(
            &[
                &[
                    "diff",
                    "--cached",
                    "--name-only",
                    "-z",
                    "--no-renames",
                    "--no-ext-diff",
                    "--",
                ][..],
                &paths,
            ]
            .concat(),
        )?;
        let paths: Vec<&str> = staged
            .split(|b| *b == 0)
            .filter(|path| !path.is_empty())
            .filter_map(|path| str::from_utf8(path).ok())
            .collect();
        if paths.is_empty() {
            return Ok(());
        }
        self.run(
            &[
                &["commit", "-q", "--no-verify", "-m", message, "--"][..],
                &paths,
            ]
            .concat(),
        )
        .map(drop)
    }

    #[cfg(test)]
    fn local_env_vars(&self) -> Result<Vec<u8>, Failure> {
        self.run(&["rev-parse", "--local-env-vars"])
    }
}

/// The stdout of a git that succeeded, else the line of what it said that
/// tells why: a push names the remote first and the reason on its ` ! `
/// line, everything else leads with the reason, after any `hint:` lines.
fn succeeded(args: &[&str], output: Output) -> Result<Vec<u8>, Failure> {
    let Output {
        status,
        stdout,
        stderr,
    } = output;
    if status.success() {
        Ok(stdout)
    } else {
        let said = String::from_utf8_lossy(&stderr);
        // `hint:` lines are advice on what to type next, never the reason.
        let first_line = said
            .lines()
            .find(|line| line.starts_with(" ! "))
            .or_else(|| said.lines().find(|line| !line.starts_with("hint:")))
            .unwrap_or_default()
            .trim();
        Err(Failure::Said(if first_line.is_empty() {
            format!("git {} failed ({status})", args.join(" "))
        } else {
            first_line.to_owned()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A variable a newer git adds and this list lacks would be inherited by
    /// every call. One the installed git does not know yet is removed for
    /// nothing, which is fine: the list may be ahead of an older git.
    #[test]
    fn every_local_variable_of_the_installed_git_is_removed() {
        let listed = Git::at(Path::new("."))
            .local_env_vars()
            .expect("git rev-parse --local-env-vars");
        let listed = String::from_utf8(listed).unwrap();
        assert!(listed.lines().count() >= 10, "{listed}");
        for variable in listed.lines() {
            assert!(
                LOCAL_ENV_VARS.contains(&variable),
                "git clears {variable} for a new repository and LOCAL_ENV_VARS lacks it"
            );
        }
    }
}
