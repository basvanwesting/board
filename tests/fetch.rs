//! The TUI's background fetch, against a real remote: it fetches and
//! counts, under the clone's lock, skips when a write holds the lock, and
//! never commits or pushes (SPEC.md, Sync). Against an ssh remote, with a
//! fake `ssh` first on `PATH`: ssh has no terminal to ask on, the user's
//! ssh command is the one that runs, and a fetch whose process is gone
//! (the TUI quit) holds the lock until its git ends.
//!
//! The fetch is a library call, so its git inherits this process's
//! environment, where the binary's tests clear it per process. This test is
//! its own `main` (`harness = false`, one thread), so it sets the
//! environment once, before anything else runs, to keep the developer's
//! home and git configuration out.

mod common;

use std::fs::{self, File, OpenOptions, TryLockError};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use board::{Board, Counts, Env, Fetched, Fetcher};
use common::{Machine, World};

/// Set on this test run again as a child: it only fetches, with a's
/// home in the variable.
const CHILD: &str = "BOARD_FETCH_TEST_CHILD";
/// Where the fake `ssh` writes its arguments, and its fifo.
const SSH_DIR: &str = "BOARD_FETCH_TEST_SSH";
/// Set: the fake `ssh` waits to be released before it fails.
const SSH_HOLD: &str = "BOARD_FETCH_TEST_SSH_HOLD";

/// What a fake `ssh` does: it notes its arguments, its process group and
/// its parent (git, which a new session makes a group leader), and the
/// askpass setting, and fails as an unreachable host does. Held, it first
/// says it has started and waits on a fifo that it makes before it says
/// so.
const FAKE_SSH: &str = r#"#!/bin/sh
printf '%s\n' "$@" >"$BOARD_FETCH_TEST_SSH/args"
printf '%s %s %s\n' "$(ps -o pgid= -p $$ | tr -d ' ')" "$PPID" \
    "$SSH_ASKPASS_REQUIRE" >"$BOARD_FETCH_TEST_SSH/session"
if [ -n "$BOARD_FETCH_TEST_SSH_HOLD" ]; then
    mkfifo "$BOARD_FETCH_TEST_SSH/release"
    : >"$BOARD_FETCH_TEST_SSH/started"
    read -r _ <"$BOARD_FETCH_TEST_SSH/release"
fi
exit 255
"#;

/// A machine's fetcher, from its home: what a child knows.
fn fetcher(home: &Path) -> Fetcher {
    let data = home.join("Work").join("board-data");
    Board::open(
        Env {
            home: home.to_owned(),
            root: data,
            cwd: home.join("Work").join("proj"),
            board: None,
        },
        Box::new(|_| {}),
    )
    .expect("the machine's board")
    .fetcher()
}

fn main() {
    if let Some(home) = std::env::var_os(CHILD) {
        // Fetches until it is killed: its ssh waits.
        fetcher(&PathBuf::from(home)).fetch();
        return;
    }
    let world = World::new();
    let a = world.a();
    let b = world.b();
    let bin = world.dir().join("bin");
    fs::create_dir(&bin).expect("mkdir");
    let ssh = bin.join("ssh");
    fs::write(&ssh, FAKE_SSH).expect("write the fake ssh");
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = std::env::var_os("PATH").expect("PATH");
    let path =
        std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(&path)))
            .expect("a PATH");
    // SAFETY: `harness = false`: no other thread exists yet to read the
    // environment while it is written.
    unsafe {
        std::env::set_var("HOME", a.home());
        std::env::set_var("GIT_CONFIG_SYSTEM", "/dev/null");
        std::env::set_var("GIT_CONFIG_GLOBAL", world.dir().join("gitconfig"));
        std::env::set_var("GIT_CEILING_DIRECTORIES", world.dir());
        std::env::set_var("PATH", path);
        std::env::set_var(SSH_DIR, &bin);
        // The fake `ssh` is the one that runs, found on `PATH`.
        std::env::remove_var("GIT_SSH_COMMAND");
        std::env::remove_var("GIT_SSH");
    }
    let fetcher = fetcher(&a.home());

    // a: one commit not pushed (a drop offline); b: one the remote has
    // and a has not fetched; a: a hand edit, not committed.
    a.board(&["add", "backlog", "one"]).ok();
    a.set_online(false);
    let dropped = a.board(&["drop", "b1"]);
    assert_eq!(dropped.code, 0, "{dropped:?}");
    a.set_online(true);
    b.board(&["add", "backlog", "two"]).ok();
    a.write("proj/chores.md", "## home\n- c1 typed by hand. Check: x\n");
    let main = a.main();
    let remote = world.remote_main();
    let status = a.status();
    assert_ne!(status, "");

    // A write holds the lock: nothing runs, so nothing is counted.
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(a.data().join(".git").join("board.lock"))
        .expect("open the lock");
    lock.lock().expect("take the lock");
    assert_eq!(fetcher.fetch(), Fetched::Busy);
    drop(lock);

    let counted = Some(Counts {
        unpushed: 1,
        behind: 1,
    });
    assert_eq!(
        fetcher.fetch(),
        Fetched::Ran {
            fetched: Ok(()),
            counts: counted,
        }
    );
    // Only a fetch: no commit, no push, the hand edit left as it was.
    assert_eq!(a.main(), main);
    assert_eq!(world.remote_main(), remote);
    assert_eq!(a.status(), status);

    // Out of reach: git's reason, and the counts against what was last
    // fetched, which is when they matter most.
    a.set_online(false);
    match fetcher.fetch() {
        Fetched::Ran {
            fetched: Err(why),
            counts,
        } => {
            assert!(why.starts_with("fatal: "), "{why}");
            assert_eq!(counts, counted);
        }
        other => panic!("{other:?}"),
    }

    over_ssh(&a, &bin);
}

/// Against an ssh remote: ssh has no terminal and no askpass, the user's
/// ssh command is the one that runs, and a fetch outlived by its process
/// holds the lock until its git ends.
fn over_ssh(a: &Machine, bin: &Path) {
    a.git(
        &a.data(),
        [
            "remote",
            "set-url",
            "origin",
            "ssh://example.invalid/board-data.git",
        ],
    );
    match fetcher(&a.home()).fetch() {
        Fetched::Ran {
            fetched: Err(_), ..
        } => {}
        other => panic!("{other:?}"),
    }
    let session = fs::read_to_string(bin.join("session")).expect("ssh ran");
    let [group, parent, askpass] = session.split_whitespace().collect::<Vec<_>>()[..] else {
        panic!("{session}");
    };
    // Its own session leads its own group: this test's is another.
    assert_eq!(group, parent, "git is not in a session of its own");
    assert_eq!(askpass, "never");

    // The user's ssh command, as it picks the key for the host.
    a.git(
        &a.data(),
        ["config", "core.sshCommand", "ssh -i ~/.ssh/id_board"],
    );
    match fetcher(&a.home()).fetch() {
        Fetched::Ran {
            fetched: Err(_), ..
        } => {}
        other => panic!("{other:?}"),
    }
    let args = fs::read_to_string(bin.join("args")).expect("ssh ran");
    assert!(
        args.lines().any(|arg| arg.ends_with("/.ssh/id_board")),
        "{args}"
    );
    a.git(&a.data(), ["config", "--unset", "core.sshCommand"]);

    #[expect(
        clippy::disallowed_methods,
        reason = "this test, run again as the process the TUI is: killed while it fetches"
    )]
    let mut child = Command::new(std::env::current_exe().expect("the test's path"))
        .env(CHILD, a.home())
        .env(SSH_HOLD, "1")
        .stdin(Stdio::null())
        .spawn()
        .expect("run the child");
    // A bounded poll: nothing tells this process that ssh started.
    let deadline = Instant::now() + Duration::from_secs(60);
    while !bin.join("started").exists() {
        let exited = child.try_wait().expect("the child");
        assert!(exited.is_none(), "the child ended first: {exited:?}");
        assert!(Instant::now() < deadline, "ssh did not start");
        std::thread::sleep(Duration::from_millis(10));
    }
    child.kill().expect("kill the child");
    child.wait().expect("wait for the child");
    let lock = File::options()
        .write(true)
        .open(a.data().join(".git").join("board.lock"))
        .expect("open the lock");
    let held = matches!(lock.try_lock(), Err(TryLockError::WouldBlock));
    // Released before the check, so a failure leaves no ssh waiting.
    fs::write(bin.join("release"), "\n").expect("release ssh");
    assert!(held, "the lock went with the process, not with its git");
    // Its git, failed with its ssh, lets go.
    lock.lock().expect("take the lock");
}
