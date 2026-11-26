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

/// What the TUI writes to turn the terminal's mouse capture on or off.
fn mouse(on: bool) -> Vec<u8> {
    use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
    let mut bytes = Vec::new();
    if on {
        ratatui::crossterm::execute!(bytes, EnableMouseCapture)
    } else {
        ratatui::crossterm::execute!(bytes, DisableMouseCapture)
    }
    .expect("into a Vec");
    bytes
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
    // An editor that notes its arguments and adds a line to the file's body.
    let editor_args = world.dir().join("editor-args");
    let editor = bin.join("editor");
    script(
        &editor,
        &format!(
            "printf '%s\\n' \"$@\" >{}\n\
             printf '\\n%s\\n' 'typed in the editor' >>\"$1\"\n",
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
    assert_eq!(
        world.remote_files("proj/chores"),
        [
            "proj/chores/c1.md",
            "proj/chores/c3.md",
            "proj/chores/c4.md"
        ]
    );
    assert_eq!(world.remote_log()[0], "proj: done c2");
    assert_eq!(
        a.file("proj/backlog/b1.md"),
        world.remote_file("proj/backlog/b1.md")
    );
    b.board(&["sync"]).ok();
    assert_eq!(
        b.board(&["ls", "chores"]).ok(),
        a.board(&["ls", "chores"]).ok()
    );

    // x, then a no, an Esc, a ctrl-c: nothing runs.
    let before = world.remote_main();
    tui(&a, "3xn3x\x1b3x\x03");
    assert_eq!(world.remote_main(), before);

    // J/K: no keys of the TUI's since items have no order; nothing runs.
    let before = world.remote_main();
    tui(&a, "2JJKJ");
    assert_eq!(world.remote_main(), before);
    assert_eq!(
        a.board(&["ls", "chores"]).ok(),
        "## home\nc1 first. Check: x\nc3 third. Check: x\nc4 fourth. Check: x\n"
    );

    // e: the editor on c1's own file; on return a sync commits what it
    // added. The mouse is captured while the TUI runs and given back
    // around the editor.
    let raw = tui(&a, "2gge");
    let (on, off) = (mouse(true), mouse(false));
    assert_eq!(raw, [&on[..], &off, &on, &off].concat());
    let args = fs::read_to_string(&editor_args).expect("the editor ran");
    assert_eq!(
        args,
        format!("{}\n", a.data().join("proj/chores/c1.md").display())
    );
    let c1 = text(world.remote_file("proj/chores/c1.md"));
    assert!(c1.ends_with("# first\n\ntyped in the editor\n"), "{c1}");
    assert_eq!(
        world.remote_log()[..2],
        ["board: hand edits", "proj: done c2"]
    );

    // y, Y: the id and the line, through OSC 52 and the clipboard helper.
    let raw = tui(&a, "3yY");
    assert_eq!(
        raw,
        [
            &on[..],
            b"\x1b]52;c;YjE=\x07\x1b]52;c;YjEgZnJvbSBi\x07",
            &off
        ]
        .concat()
    );
    assert_eq!(
        fs::read_to_string(&clipboard).expect("wl-copy ran"),
        "b1\nb1 from b\n"
    );
    // A yank writes nothing.
    assert_eq!(a.status(), "");

    // yy: the whole item, what `board show` prints, after the id of the
    // first y: a chore with a body, an open brief, a filed one. A chore is
    // its file, as a brief is.
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
    fs::write(&clipboard, "").expect("empty the clipboard");
    tui(&a, "2/Wash\ryy");
    let file = text(a.file(&format!("proj/chores/{chore}.md")));
    assert!(file.starts_with(&format!("id: {chore}\n")), "{file}");
    assert_eq!(
        fs::read_to_string(&clipboard).expect("wl-copy ran"),
        format!("{chore}\n{file}\n")
    );
    assert_eq!(a.status(), "");
}
