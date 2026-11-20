//! The one place that spawns `git` (`clippy.toml` refuses `Command::new`
//! everywhere else), so every call gets the same clean surroundings.

use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

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

    /// Runs `git <args>` in the directory and gives its stdout.
    fn run<const N: usize>(&self, args: [&str; N]) -> Result<Vec<u8>, Failure> {
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
            // What the command reads is not git's to consume.
            .stdin(Stdio::null());
        for setting in CONFIG {
            command.args(["-c", setting]);
        }
        command.arg("-C").arg(&self.dir).args(args);
        let Output {
            status,
            stdout,
            stderr,
        } = command.output().map_err(Failure::NotRun)?;
        if status.success() {
            Ok(stdout)
        } else {
            let said = String::from_utf8_lossy(&stderr);
            let first_line = said.lines().next().unwrap_or_default().trim();
            Err(Failure::Said(if first_line.is_empty() {
                format!("git {} failed ({status})", args.join(" "))
            } else {
                first_line.to_owned()
            }))
        }
    }

    /// Runs a `git <args>` that prints one path.
    fn path<const N: usize>(&self, args: [&str; N]) -> Result<PathBuf, Failure> {
        let stdout = self.run(args)?;
        // One line; a path may hold any other byte.
        let path = stdout.strip_suffix(b"\n").unwrap_or(&stdout);
        Ok(PathBuf::from(OsString::from_vec(path.to_vec())))
    }

    /// The top directory of the work tree the directory is in.
    pub(crate) fn toplevel(&self) -> Result<PathBuf, Failure> {
        self.path(["rev-parse", "--show-toplevel"])
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
        let own = self.path(["rev-parse", "--path-format=absolute", "--git-dir"])?;
        let shared = self.path(["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        Ok((own != shared).then_some(shared))
    }

    #[cfg(test)]
    fn local_env_vars(&self) -> Result<Vec<u8>, Failure> {
        self.run(["rev-parse", "--local-env-vars"])
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
