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
//!
//! Faults are made the way git makes them, with hooks, and nothing is timed:
//!
//! - a remote that refuses pushes: a `pre-receive` hook that counts down;
//! - something happening while a write pushes: a one-shot `pre-push` hook in
//!   the machine's clone (it deletes itself first, so a retry runs without
//!   it);
//! - a remote that is gone: `origin` pointing at a path with nothing there;
//! - a file saved while a write pushes: a `pre-push` hook that writes it.

#![allow(dead_code, reason = "each test crate uses a part of the harness")]

use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};

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
        let output = self.git_output(dir, args);
        assert!(
            output.status.success(),
            "git failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Runs git and gives what it did, failed or not.
    fn git_output<S: AsRef<OsStr>>(&self, dir: &Path, args: impl IntoIterator<Item = S>) -> Output {
        #[expect(
            clippy::disallowed_methods,
            reason = "the harness builds repositories around the product, not through it"
        )]
        let mut command = Command::new("git");
        self.surroundings(&mut command, &self.root);
        command
            .current_dir(dir)
            .args(args)
            .output()
            .expect("run git")
    }

    /// The temp dir: room for the files a test's hooks write.
    pub fn dir(&self) -> &Path {
        &self.root
    }

    /// The bare repository both clones push to.
    pub fn remote(&self) -> PathBuf {
        self.root.join("remote.git")
    }

    /// The commit the remote's `main` is on.
    pub fn remote_main(&self) -> String {
        rev(&self.git_output(&self.remote(), ["rev-parse", "main"]))
    }

    /// A file of the remote's `main`; `None` when it has none.
    pub fn remote_file(&self, file: &str) -> Option<Vec<u8>> {
        let output = self.git_output(
            &self.remote(),
            ["cat-file", "blob", &format!("main:{file}")],
        );
        output.status.success().then_some(output.stdout)
    }

    /// The files below `dir` in the remote's `main`, relative to the top.
    pub fn remote_files(&self, dir: &str) -> Vec<String> {
        let output = self.git_output(
            &self.remote(),
            [
                "ls-tree",
                "-r",
                "--name-only",
                "main",
                "--",
                &format!("{dir}/"),
            ],
        );
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The files below `dir` in the remote's `main` with their bytes, by
    /// path: two git processes however many files, for a test that reads a
    /// whole board after every step.
    pub fn remote_blobs(&self, dir: &str) -> Vec<(String, Vec<u8>)> {
        let listed = self.git_output(
            &self.remote(),
            ["ls-tree", "-r", "main", "--", &format!("{dir}/")],
        );
        assert!(listed.status.success());
        let entries: Vec<(String, String)> = String::from_utf8(listed.stdout)
            .unwrap()
            .lines()
            .map(|line| {
                let (meta, path) = line.split_once('\t').expect("<mode> <type> <id>\t<path>");
                let id = meta.split(' ').nth(2).expect("an object id");
                (path.to_owned(), id.to_owned())
            })
            .collect();
        if entries.is_empty() {
            return Vec::new();
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "the harness reads the remote around the product, not through it"
        )]
        let mut command = Command::new("git");
        self.surroundings(&mut command, &self.root);
        let mut child = command
            .current_dir(self.remote())
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("run git");
        let input: String = entries.iter().map(|(_, id)| format!("{id}\n")).collect();
        let mut stdin = child.stdin.take().expect("stdin was piped");
        // Written from a thread: git answers as it reads, and an answer
        // larger than the pipe would stop it before it read the rest.
        let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
        let output = child.wait_with_output().expect("wait for git");
        writer.join().unwrap().expect("write to git");
        assert!(output.status.success());
        // `<id> blob <size>\n<bytes>\n` per object, in the order asked.
        let mut rest = output.stdout.as_slice();
        entries
            .into_iter()
            .map(|(path, _)| {
                let end = rest
                    .iter()
                    .position(|b| *b == b'\n')
                    .expect("a header line");
                let header = String::from_utf8(rest[..end].to_vec()).unwrap();
                let size: usize = header.rsplit(' ').next().unwrap().parse().expect("a size");
                let bytes = rest[end + 1..end + 1 + size].to_vec();
                rest = &rest[end + 1 + size + 1..];
                (path, bytes)
            })
            .collect()
    }

    /// The files the remote's commit `rev` changed, from its first parent.
    pub fn remote_changed(&self, rev: &str) -> Vec<String> {
        let output = self.git_output(
            &self.remote(),
            [
                "diff",
                "--name-only",
                "--no-renames",
                &format!("{rev}^"),
                rev,
            ],
        );
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The subjects of the remote's commits, newest first.
    pub fn remote_log(&self) -> Vec<String> {
        let output = self.git_output(&self.remote(), ["log", "--format=%s", "main"]);
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// How many pushes the remote still refuses.
    pub fn rejects_left(&self) -> u32 {
        match fs::read_to_string(self.root.join("rejects")) {
            Ok(count) => count.trim().parse().expect("a count"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => panic!("read the counter: {error}"),
        }
    }

    /// From now on the remote refuses the next `pushes` pushes, whoever
    /// makes them.
    pub fn reject_pushes(&self, pushes: u32) {
        let counter = self.root.join("rejects");
        fs::write(&counter, format!("{pushes}\n")).expect("write the counter");
        install_hook(
            &self.remote().join("hooks").join("pre-receive"),
            &format!(
                "n=$(cat {counter})\n\
                 if [ \"$n\" -gt 0 ]; then\n\
                 \techo $((n - 1)) > {counter}\n\
                 \techo 'refused by the test' >&2\n\
                 \texit 1\n\
                 fi\n",
                counter = quote(&counter.to_string_lossy())
            ),
        );
    }
}

/// The commit id git printed.
fn rev(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone())
        .unwrap()
        .trim()
        .to_owned()
}

/// Writes an executable `sh` script.
fn install_hook(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}")).expect("write a hook");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("make a hook executable");
}

/// A word for `sh`, as it is.
pub fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// `pre-push` bodies. A `pre-push` hook reads `<local ref> <local sha>
/// <remote ref> <remote sha>` lines; its parent is the `git push` that
/// `board` started, its grandparent `board`.
pub mod pre_push {
    /// The push reaches the remote, and the answer is lost: the hook pushes
    /// the commit itself and then fails the push it was asked about.
    pub const LOST_ACK: &str = "\
read -r _ sha _ _
git push -q --no-verify origin \"$sha:refs/heads/main\" >/dev/null 2>&1
exit 1
";

    /// `board` dies before its push leaves.
    pub const KILL_BEFORE_PUSH: &str = "\
kill -KILL $(ps -o ppid= -p $PPID)
exit 1
";

    /// `board` dies right after its push reached the remote, before it hears
    /// of it.
    pub const KILL_AFTER_PUSH: &str = "\
read -r _ sha _ _
git push -q --no-verify origin \"$sha:refs/heads/main\" >/dev/null 2>&1
kill -KILL $(ps -o ppid= -p $PPID)
exit 1
";

    /// Where [`late_landing`] keeps the first push's commit, in the dir it
    /// is given, until the second push.
    pub const LATE_LANDING_FILE: &str = "late-landing";

    /// For [`super::Machine::hook`]. The first push fails without reaching
    /// the remote; the second pushes the first's commit before it goes on,
    /// so the first lands late, after the fetch the second was built on, and
    /// the second is refused. The hook is gone after that.
    ///
    /// Between them the clone's committer changes: two runs of a command on
    /// one base in one second would otherwise make one commit, as git
    /// counts time in seconds.
    pub fn late_landing(dir: &std::path::Path) -> String {
        let saved = super::quote(&dir.join(LATE_LANDING_FILE).to_string_lossy());
        format!(
            "read -r _ sha _ _\n\
             if [ -f {saved} ]; then\n\
             \tgit push -q --no-verify origin \"$(cat {saved}):refs/heads/main\" >/dev/null 2>&1\n\
             \trm -f \"$0\" {saved}\n\
             \texit 0\n\
             fi\n\
             echo \"$sha\" >{saved}\n\
             git config user.email \"late-$sha@example.org\"\n\
             exit 1\n"
        )
    }

    /// Runs `script` (a [`super::Machine::script`]) while the push waits,
    /// with its stdout, stderr and exit code in `<dir>/window.{out,err,code}`;
    /// the push then goes on.
    pub fn window(script: &str, dir: &std::path::Path) -> String {
        let file = |name: &str| super::quote(&dir.join(name).to_string_lossy());
        format!(
            "{script} </dev/null >{out} 2>{err}\necho $? >{code}\nexit 0\n",
            out = file("window.out"),
            err = file("window.err"),
            code = file("window.code"),
        )
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

    /// Runs `board <args>` in the project's repository with `input` on
    /// stdin.
    pub fn board_with_input(&self, args: &[&str], input: &[u8]) -> Ran {
        let mut child = self
            .command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run board");
        // Written whole before the output is read: a command reads its
        // input before it prints more than the pipe holds. A command
        // refused on its arguments exits before it reads stdin, and the
        // write then meets a closed pipe: that is the command's answer,
        // judged by the caller from its status and output, not a failure of
        // the test. Rejected:
        // `board` reading stdin before its usage checks, which would change
        // the tool for its tests and wait on a terminal for input it is
        // about to refuse.
        let written = child
            .stdin
            .take()
            .expect("stdin was piped")
            .write_all(input);
        if let Err(error) = written
            && error.kind() != io::ErrorKind::BrokenPipe
        {
            panic!("write stdin: {error}");
        }
        let Output {
            status,
            stdout,
            stderr,
        } = child.wait_with_output().expect("wait for board");
        ran(status, stdout, stderr)
    }

    /// Runs `board <args>` in another directory.
    pub fn board_in(&self, directory: &Path, args: &[&str]) -> Ran {
        run(self.command().current_dir(directory).args(args))
    }

    /// Runs `board <args>`, which a hook is expected to kill.
    pub fn board_killed(&self, args: &[&str]) {
        let output = self.command().args(args).output().expect("run board");
        assert_eq!(output.status.signal(), Some(9), "{output:?}");
    }

    /// `board <args>` on this machine, as a line of `sh`: what a hook on
    /// another machine runs.
    pub fn script(&self, args: &[&str]) -> String {
        let args: Vec<String> = args.iter().map(|arg| quote(arg)).collect();
        format!(
            "(cd {proj} && HOME={home} {board} {args})",
            proj = quote(&self.proj().to_string_lossy()),
            home = quote(&self.home().to_string_lossy()),
            board = quote(env!("CARGO_BIN_EXE_board")),
            args = args.join(" ")
        )
    }

    /// [`Machine::script`] with the file `input` on stdin: the text of
    /// `amend` or `report`.
    pub fn script_with_input(&self, args: &[&str], input: &Path) -> String {
        let script = self.script(args);
        let script = script.strip_suffix(')').expect("a subshell");
        format!("{script} <{})", quote(&input.to_string_lossy()))
    }

    /// Installs a hook in the board-data clone that runs once: it deletes
    /// itself before `body` runs.
    pub fn hook_once(&self, name: &str, body: &str) {
        install_hook(
            &self.data().join(".git").join("hooks").join(name),
            &format!("rm -f \"$0\"\n{body}"),
        );
    }

    /// Installs a hook in the board-data clone; it stays until it removes
    /// itself or [`Machine::unhook`].
    pub fn hook(&self, name: &str, body: &str) {
        install_hook(&self.data().join(".git").join("hooks").join(name), body);
    }

    /// Whether a hook is installed in the board-data clone: a one-shot hook
    /// that is gone has run.
    pub fn has_hook(&self, name: &str) -> bool {
        self.data().join(".git").join("hooks").join(name).exists()
    }

    /// Points the clone's `origin` at the remote, or at a path with nothing
    /// there: a fetch then fails at once.
    pub fn set_online(&self, online: bool) {
        let url = if online {
            self.world.remote()
        } else {
            self.world.root.join("gone")
        };
        self.git(
            &self.data(),
            [
                OsStr::new("remote"),
                "set-url".as_ref(),
                "origin".as_ref(),
                url.as_ref(),
            ],
        );
    }

    /// The ref the board-data clone's `HEAD` is on; empty when detached.
    pub fn head_ref(&self) -> String {
        let output = self
            .world
            .git_output(&self.data(), ["symbolic-ref", "-q", "HEAD"]);
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    /// Removes a hook from the board-data clone, run or not.
    pub fn unhook(&self, name: &str) {
        let path = self.data().join(".git").join("hooks").join(name);
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("remove a hook: {error}"),
        }
    }

    /// A file of the board-data clone as it is on disk; `None` when it is not
    /// there.
    pub fn file(&self, file: &str) -> Option<Vec<u8>> {
        match fs::read(self.data().join(file)) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("read {file}: {error}"),
        }
    }

    /// The commit `main` of the board-data clone is on.
    pub fn main(&self) -> String {
        rev(&self.world.git_output(&self.data(), ["rev-parse", "main"]))
    }

    /// The commit `HEAD` of a repository of the machine is on.
    pub fn head_of(&self, repository: &Path) -> String {
        rev(&self.world.git_output(repository, ["rev-parse", "HEAD"]))
    }

    /// What `git status --porcelain` says of the board-data clone.
    pub fn status(&self) -> String {
        let output = self
            .world
            .git_output(&self.data(), ["status", "--porcelain", "-uall"]);
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    }

    /// Commits everything in the board-data clone by hand and pushes it.
    pub fn commit_and_push(&self, message: &str) {
        self.git(&self.data(), ["add", "-A"]);
        self.git(&self.data(), ["commit", "-q", "-m", message]);
        self.git(&self.data(), ["push", "-q", "origin", "main"]);
    }
}

/// Runs a command made by [`Machine::command`] to its end.
pub fn run(command: &mut Command) -> Ran {
    let Output {
        status,
        stdout,
        stderr,
    } = command.output().expect("run board");
    ran(status, stdout, stderr)
}

fn ran(status: ExitStatus, stdout: Vec<u8>, stderr: Vec<u8>) -> Ran {
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
