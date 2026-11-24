//! The TUI's actions through its whole runtime (worker, watcher, editor,
//! yank) and the real write loop: a remote and two clones, keys given up
//! front, a screen in memory (`board::tui::run_headless`). What comes back
//! is checked on the remote, on the other clone and in what the editor and
//! the clipboard were handed (SPEC.md, TUI).
//!
//! The TUI is a library call, so its git and its editor inherit this
//! process's environment, where the binary's tests clear it per process.
//! This test is its own `main` (`harness = false`, one thread), so it sets
//! the environment once, before anything else runs, to keep the
//! developer's home, git configuration, editor and clipboard out.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use board::{Board, Env};
use common::{Machine, World, quote};

/// The TUI on machine `machine`'s board with these keys; what it wrote to
/// the terminal outside its frames.
fn tui(machine: &Machine, keys: &str) -> Vec<u8> {
    let board = Board::open(
        Env {
            home: machine.home(),
            root: machine.data(),
            cwd: machine.proj(),
            board: None,
        },
        Box::new(|_| {}),
    )
    .expect("the machine's board");
    board::tui::run_headless(board, keys).expect("the TUI ran")
}

fn script(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}")).expect("write a script");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
}

fn text(bytes: Option<Vec<u8>>) -> String {
    String::from_utf8(bytes.expect("the file is there")).expect("UTF-8")
}

fn main() {
    let world = World::new();
    let a = world.a();
    let b = world.b();
    let bin = world.dir().join("bin");
    fs::create_dir(&bin).expect("mkdir");
    // A clipboard that keeps each text it is given, one per line.
    let clipboard = world.dir().join("clipboard");
    script(
        &bin.join("wl-copy"),
        &format!(
            "{{ cat; echo; }} >>{}\n",
            quote(&clipboard.to_string_lossy())
        ),
    );
    // An editor that notes its arguments and adds a chore by hand.
    let editor_args = world.dir().join("editor-args");
    let editor = bin.join("editor");
    script(
        &editor,
        &format!(
            "printf '%s\\n' \"$@\" >{}\n\
             printf '%s\\n' '- typed in the editor. Check: by eye' >>\"$2\"\n",
            quote(&editor_args.to_string_lossy())
        ),
    );
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
        std::env::set_var("EDITOR", &editor);
        // The fake `wl-copy` is the one that runs, found on `PATH`.
        std::env::set_var("WAYLAND_DISPLAY", "board-test");
    }

    for chore in ["first", "second", "third", "fourth"] {
        a.board(&["add", "chore", chore, "--group", "home", "--check", "x"])
            .ok();
    }
    // A commit of b's that a has not fetched: a's writes merge it.
    assert_eq!(b.board(&["add", "backlog", "from b"]).ok(), "b1\n");

    // d, y: done through the write loop, after b's commit; then the input
    // ends, and the TUI quits once the write is done.
    tui(&a, "2jdy");
    let chores = text(world.remote_file("proj/chores.md"));
    assert!(!chores.contains("c2 "), "{chores}");
    assert!(chores.contains("- c1 first") && chores.contains("- c3 third"));
    assert_eq!(world.remote_log()[0], "proj: done c2");
    assert_eq!(
        a.file("proj/backlog.md"),
        world.remote_file("proj/backlog.md")
    );
    b.board(&["sync"]).ok();
    assert_eq!(b.file("proj/chores.md"), a.file("proj/chores.md"));

    // x, then a no, an Esc, a ctrl-c: nothing runs.
    let before = world.remote_main();
    tui(&a, "3xn3x\x1b3x\x03");
    assert_eq!(world.remote_main(), before);

    // J/K: a run of presses on screen, one move when it ends (here: the
    // input ends); c1 goes below c4, past c3 and c4 and back and on.
    tui(&a, "2JJKJ");
    assert_eq!(world.remote_log()[..2], ["proj: move c1", "proj: done c2"]);
    let chores = text(world.remote_file("proj/chores.md"));
    let order: Vec<&str> = chores
        .lines()
        .filter_map(|line| line.strip_prefix("- "))
        .map(|item| item.split(' ').next().unwrap())
        .collect();
    assert_eq!(order, ["c3", "c4", "c1"]);

    // e: the editor on chores.md at c1's line; on return a sync commits
    // what it added, and gives that line an id.
    tui(&a, "2Ge");
    let args = fs::read_to_string(&editor_args).expect("the editor ran");
    let line = chores
        .lines()
        .position(|line| line.starts_with("- c1 "))
        .unwrap()
        + 1;
    assert_eq!(
        args,
        format!("+{line}\n{}\n", a.data().join("proj/chores.md").display())
    );
    let chores = text(world.remote_file("proj/chores.md"));
    assert!(
        chores.ends_with("- c5 typed in the editor. Check: by eye\n"),
        "{chores}"
    );
    assert_eq!(
        world.remote_log()[..3],
        ["proj: assign c5", "board: hand edits", "proj: move c1"]
    );

    // y, Y: the id and the line, through OSC 52 and the clipboard helper.
    let raw = tui(&a, "3yY");
    assert_eq!(raw, b"\x1b]52;c;YjE=\x07\x1b]52;c;YjEgZnJvbSBi\x07");
    assert_eq!(
        fs::read_to_string(&clipboard).expect("wl-copy ran"),
        "b1\nb1 from b\n"
    );
    // A yank writes nothing.
    assert_eq!(a.status(), "");

    // yy: the whole item, what `board show` prints, after the id of the
    // first y: a chore with a body, an open brief, a filed one.
    let chore = a
        .board_with_input(
            &[
                "add",
                "chore",
                "Wash the car",
                "--group",
                "Home",
                "--check",
                "it shines",
                "--body",
            ],
            b"first the wheels\n  - front\n  - back\n",
        )
        .ok();
    let chore = chore.trim_end();
    let path = a.board(&["new", "brief", "yank-me"]).ok();
    let open = path
        .trim_end()
        .rsplit('/')
        .next()
        .unwrap()
        .split('-')
        .next()
        .unwrap()
        .to_owned();
    a.write(
        "proj/briefs/done/77-old-one.md",
        "id: 77\nstatus: filed\n\n# Old\n## Report\nall done\n",
    );
    a.board(&["sync"]).ok();
    for (keys, id) in [
        ("2/Wash\ryy", chore),
        ("1/yank-me\ryy", open.as_str()),
        ("1]/old-one\ryy", "77"),
    ] {
        fs::write(&clipboard, "").expect("empty the clipboard");
        tui(&a, keys);
        let show = a.board(&["show", id]).ok();
        assert_eq!(
            fs::read_to_string(&clipboard).expect("wl-copy ran"),
            format!("{id}\n{show}\n"),
            "{keys:?}"
        );
    }
    assert_eq!(a.status(), "");
}
