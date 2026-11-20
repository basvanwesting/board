//! The world a test runs in: a remote and two machines, all in one temp dir.
//!
//! ```text
//! tmp/remote.git                  bare, one commit
//! tmp/{a,b}/Work/board-data       a clone each
//! tmp/{a,b}/Work/proj             the project: a git repo with a commit
//! tmp/gitconfig
//! ```
//!
//! A machine is its `HOME`: that one variable moves `~/Work` and the default
//! board-data clone together, so the tests reach the product through what a
//! user sets and the product has no seam for them. Every process starts
//! with a cleared environment, so nothing of the developer's git
//! configuration, home or board gets in.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

pub struct World {
    /// Removed with the world.
    _tmp: TempDir,
    /// The temp dir, canonical: what the product prints, and what git
    /// compares `GIT_CEILING_DIRECTORIES` with.
    root: PathBuf,
}

impl World {
    pub fn new() -> World {
        let tmp = tempfile::tempdir().expect("a temp dir");
        let root = fs::canonicalize(tmp.path()).expect("the temp dir's path");
        let world = World { _tmp: tmp, root };
        fs::write(
            world.root.join("gitconfig"),
            "[user]\n\tname = Test\n\temail = test@example.invalid\n\
             [gc]\n\tauto = 0\n\
             [maintenance]\n\tauto = false\n",
        )
        .expect("write gitconfig");

        let remote = world.root.join("remote.git");
        let a = world.a();
        let b = world.b();
        fs::create_dir_all(a.home().join("Work")).expect("mkdir");
        fs::create_dir_all(b.home().join("Work")).expect("mkdir");
        world.git(
            &world.root,
            ["init", "-q", "--bare", "-b", "main", "remote.git"],
        );
        world.git(
            &a.home().join("Work"),
            ["init", "-q", "-b", "main", "board-data"],
        );
        world.git(&a.data(), ["commit", "-q", "--allow-empty", "-m", "start"]);
        world.git(
            &a.data(),
            [
                OsStr::new("remote"),
                "add".as_ref(),
                "origin".as_ref(),
                remote.as_ref(),
            ],
        );
        world.git(&a.data(), ["push", "-q", "-u", "origin", "main"]);
        world.git(
            &b.home().join("Work"),
            [
                OsStr::new("clone"),
                "-q".as_ref(),
                remote.as_ref(),
                "board-data".as_ref(),
            ],
        );
        for machine in [&a, &b] {
            machine.repository("Work/proj");
            world.git(
                &machine.proj(),
                ["commit", "-q", "--allow-empty", "-m", "start"],
            );
        }
        world
    }

    pub fn a(&self) -> Machine<'_> {
        Machine {
            world: self,
            name: "a",
        }
    }

    pub fn b(&self) -> Machine<'_> {
        Machine {
            world: self,
            name: "b",
        }
    }

    /// The environment of every process of the world, on machine `home`.
    fn surroundings(&self, command: &mut Command, home: &Path) {
        command
            .env_clear()
            .env("HOME", home)
            .env("PATH", std::env::var_os("PATH").expect("PATH"))
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            // A temp dir inside some repository must not make every
            // directory of the world part of that repository.
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .stdin(Stdio::null());
    }

    /// Runs git for the setup of a test; it has to succeed.
    fn git<S: AsRef<OsStr>>(&self, dir: &Path, args: impl IntoIterator<Item = S>) {
        #[expect(
            clippy::disallowed_methods,
            reason = "the harness builds repositories around the product, not through it"
        )]
        let mut command = Command::new("git");
        self.surroundings(&mut command, &self.root);
        let output = command
            .current_dir(dir)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// One machine of the world: a home with its own clone and project.
pub struct Machine<'w> {
    world: &'w World,
    name: &'static str,
}

impl Machine<'_> {
    pub fn home(&self) -> PathBuf {
        self.world.root.join(self.name)
    }

    /// The board-data clone.
    pub fn data(&self) -> PathBuf {
        self.home().join("Work").join("board-data")
    }

    /// The project's repository; its board is `proj`.
    pub fn proj(&self) -> PathBuf {
        self.home().join("Work").join("proj")
    }

    /// Writes a file of the board-data clone, as an editor would.
    pub fn write(&self, file: &str, content: impl AsRef<[u8]>) {
        let path = self.data().join(file);
        fs::create_dir_all(path.parent().expect("a file in the clone")).expect("mkdir");
        fs::write(path, content).expect("write");
    }

    /// Makes a directory below the home a git repository and gives its path.
    pub fn repository(&self, directory: &str) -> PathBuf {
        let path = self.home().join(directory);
        fs::create_dir_all(&path).expect("mkdir");
        self.world.git(&path, ["init", "-q", "-b", "main"]);
        path
    }

    /// Runs git in a directory for the setup of a test, as the machine's
    /// user would by hand; it has to succeed.
    pub fn git<S: AsRef<OsStr>>(&self, directory: &Path, args: impl IntoIterator<Item = S>) {
        self.world.git(directory, args);
    }

    /// `board`, to be run on this machine in the project's repository.
    pub fn command(&self) -> Command {
        #[expect(
            clippy::disallowed_methods,
            reason = "the tests run the binary as a user does"
        )]
        let mut command = Command::new(env!("CARGO_BIN_EXE_board"));
        self.world.surroundings(&mut command, &self.home());
        command.current_dir(self.proj());
        command
    }

    /// Runs `board <args>` in the project's repository.
    pub fn board(&self, args: &[&str]) -> Ran {
        run(self.command().args(args))
    }

    /// Runs `board <args>` in another directory.
    pub fn board_in(&self, directory: &Path, args: &[&str]) -> Ran {
        run(self.command().current_dir(directory).args(args))
    }
}

/// Runs a command made by [`Machine::command`] to its end.
pub fn run(command: &mut Command) -> Ran {
    let Output {
        status,
        stdout,
        stderr,
    } = command.output().expect("run board");
    Ran {
        // No code: killed by a signal, which no test expects.
        code: status
            .code()
            .unwrap_or_else(|| panic!("board ended by {status}")),
        stdout,
        stderr: String::from_utf8(stderr).expect("stderr is UTF-8"),
    }
}

/// How a `board` run ended.
#[derive(Debug)]
pub struct Ran {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl Ran {
    /// The stdout of a run that succeeded and had nothing else to say.
    #[track_caller]
    pub fn ok(self) -> String {
        self.out(0)
    }

    /// The stdout of a run that ended with `code` and nothing on stderr.
    #[track_caller]
    pub fn out(self, code: i32) -> String {
        assert_eq!((self.code, self.stderr.as_str()), (code, ""), "{self:?}");
        String::from_utf8(self.stdout).expect("stdout is UTF-8")
    }

    /// The stderr of a run that ended with `code` and printed no result.
    #[track_caller]
    pub fn failed(self, code: i32) -> String {
        assert_eq!(
            (self.code, self.stdout.as_slice()),
            (code, &b""[..]),
            "{self:?}"
        );
        self.stderr
    }
}
