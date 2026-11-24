//! The write loop through the binary, one test per mechanism: two clones of
//! one bare remote, faults made with git hooks.

mod common;

use std::fs;
use std::io;

use common::{Machine, World, pre_push, quote, run};

fn text(bytes: Option<Vec<u8>>) -> String {
    String::from_utf8(bytes.expect("the file is there")).unwrap()
}

/// How often `needle` is in the remote's file.
fn on_remote(world: &World, file: &str, needle: &str) -> usize {
    world.remote_file(file).map_or(0, |bytes| {
        String::from_utf8(bytes).unwrap().matches(needle).count()
    })
}

/// The clone is where the remote is, with nothing changed by hand.
#[track_caller]
fn in_step(world: &World, machine: &Machine) {
    assert_eq!(machine.main(), world.remote_main());
    assert_eq!(machine.status(), "");
}

// ---- the commands ----

#[test]
fn add_prints_the_new_id_and_pushes_one_commit_with_the_one_file() {
    let world = World::new();
    let a = world.a();
    assert_eq!(a.board(&["add", "backlog", "Dark mode"]).ok(), "b1\n");
    assert_eq!(
        a.board(&["add", "backlog", "Faster search", "--from", "126"])
            .ok(),
        "b2\n"
    );
    assert_eq!(
        a.board(&[
            "add",
            "chore",
            "Rotate the key",
            "--group",
            "Home",
            "--check",
            "login works",
            "--from",
            "b2"
        ])
        .ok(),
        "c1\n"
    );
    assert_eq!(
        text(world.remote_file("proj/backlog.md")),
        "- b1 Dark mode\n- b2 Faster search  (from 126)\n"
    );
    assert_eq!(
        text(world.remote_file("proj/chores.md")),
        "## Home\n- c1 Rotate the key. Check: login works  (b2)\n"
    );
    assert_eq!(
        world.remote_log(),
        ["proj: add c1", "proj: add b2", "proj: add b1", "start"]
    );
    // The clone's files are the remote's.
    in_step(&world, &a);
    assert_eq!(
        a.board(&["ls", "backlog"]).ok(),
        "b1 Dark mode\nb2 Faster search  (from 126)\n"
    );
}

#[test]
fn done_and_drop_delete_lines_all_or_nothing() {
    let world = World::new();
    let a = world.a();
    for chore in ["one", "two", "three"] {
        a.board(&["add", "chore", chore, "--group", "Home", "--check", "x"])
            .ok();
    }
    a.board(&["add", "backlog", "later"]).ok();
    assert_eq!(
        a.board(&["done", "c3", "c9"]).failed(1),
        "board: done c9: no such chore\n"
    );
    assert_eq!(a.board(&["done", "c3", "c1", "c3"]).ok(), "done c3 c1\n");
    assert_eq!(
        a.board(&["drop", "b2"]).failed(1),
        "board: drop b2: no such backlog item\n"
    );
    assert_eq!(a.board(&["drop", "b1"]).ok(), "dropped b1\n");
    assert_eq!(
        text(world.remote_file("proj/chores.md")),
        "## Home\n- c2 two. Check: x\n"
    );
    assert_eq!(text(world.remote_file("proj/backlog.md")), "");
    assert_eq!(
        world.remote_log()[..2],
        ["proj: dropped b1", "proj: done c3 c1"]
    );
    in_step(&world, &a);
}

/// `retitle` changes an item's line in one commit and keeps its id, place,
/// reference and body; a chore's check stays unless one is given.
#[test]
fn retitle_changes_an_item_s_line_and_keeps_the_rest() {
    let world = World::new();
    let a = world.a();
    a.board_with_input(
        &[
            "add",
            "chore",
            "Rotate the key",
            "--group",
            "Home",
            "--check",
            "login works",
            "--from",
            "126",
            "--body",
        ],
        b"the steps",
    )
    .ok();
    a.board(&["add", "chore", "Other", "--group", "Home", "--check", "x"])
        .ok();
    a.board(&["add", "backlog", "Dark mode", "--from", "c1"])
        .ok();
    assert_eq!(
        a.board(&["retitle", "c1", "Rotate both keys"]).ok(),
        "retitled c1\n"
    );
    assert_eq!(
        a.board(&["show", "c1"]).ok(),
        "c1 Rotate both keys. Check: login works  (126)\n  the steps\n"
    );
    a.board(&["retitle", "c1", "--check", "the old key fails"])
        .ok();
    a.board(&["retitle", "b1", "Dark theme"]).ok();
    assert_eq!(
        text(world.remote_file("proj/chores.md")),
        "## Home\n- c1 Rotate both keys. Check: the old key fails  (126)\n  the steps\n- c2 Other. Check: x\n"
    );
    assert_eq!(
        text(world.remote_file("proj/backlog.md")),
        "- b1 Dark theme  (from c1)\n"
    );
    assert_eq!(
        world.remote_log()[..3],
        ["proj: retitle b1", "proj: retitle c1", "proj: retitle c1"]
    );
    assert_eq!(a.board(&["lint"]).ok(), "ok\n");
    in_step(&world, &a);
}

#[test]
fn retitle_refuses_what_names_no_line_or_no_change() {
    let world = World::new();
    let a = world.a();
    a.board(&[
        "add",
        "chore",
        "Rotate the key",
        "--group",
        "Home",
        "--check",
        "x",
    ])
    .ok();
    a.board(&["add", "backlog", "Dark mode"]).ok();
    a.write(
        "proj/chores.md",
        "## Home\n- c1 Rotate the key. Check: x\n- c2 by hand, no check\n",
    );
    // The sync commits the hand edit; lint notes the chore without a check.
    assert_eq!(a.board(&["sync"]).code, 0);
    for args in [
        &["retitle", "c1"][..],
        &["retitle", "128", "x"],
        &["retitle", "b1", "--check", "y"],
    ] {
        a.board(args).failed(2);
    }
    assert!(
        a.board(&["retitle", "128", "x"])
            .failed(2)
            .contains("a brief's title is in its file")
    );
    for (args, message) in [
        (
            &["retitle", "c9", "x"][..],
            "board: retitle c9: no such chore\n",
        ),
        (
            &["retitle", "c1", "--check", "(126)"],
            "board: retitle c1: `(126)` does not say how to see that the chore is done\n",
        ),
        (
            &["retitle", "c1", "--check", "the Check: tab"],
            "board: retitle c1: `the Check: tab` holds a `Check:` of its own; lint would read only \
             what follows it\n",
        ),
        (
            &["retitle", "c2", "y"],
            "board: retitle c2: its line has no check to keep; give one with --check\n",
        ),
    ] {
        // After the note lint gives on every write here.
        let said = a.board(args).failed(1);
        assert!(said.ends_with(message), "{args:?}: {said}");
    }
    assert_eq!(
        text(world.remote_file("proj/chores.md")),
        "## Home\n- c1 Rotate the key. Check: x\n- c2 by hand, no check\n"
    );
}

/// A body is written through `board`: given at `add --body`, appended by
/// `amend`, replaced by `amend --replace`; `show` gives it back and `done`
/// takes it along.
#[test]
fn add_body_amend_and_replace_write_an_item_s_body() {
    let world = World::new();
    let a = world.a();
    let body = b"Run the query:\n\n- a list\n  - nested   \n";
    assert_eq!(
        a.board_with_input(
            &[
                "add",
                "chore",
                "Rotate the key",
                "--group",
                "Home",
                "--check",
                "the query below returns nothing",
                "--body"
            ],
            body,
        )
        .ok(),
        "c1\n"
    );
    a.board_with_input(
        &["add", "backlog", "Dark mode", "--body"],
        b"why: the night shift",
    )
    .ok();
    assert_eq!(
        a.board_with_input(
            &["amend", "c1"],
            b"And then:\n## not a heading\n- c9 not an item\n"
        )
        .ok(),
        "amended c1\n"
    );
    let chore = "c1 Rotate the key. Check: the query below returns nothing\n  Run the query:\n  \
                 - a list\n    - nested\n  And then:\n  ## not a heading\n  - c9 not an item\n";
    assert_eq!(a.board(&["show", "c1"]).ok(), chore);
    assert_eq!(
        text(world.remote_file("proj/chores.md")),
        format!("## Home\n- {chore}")
    );
    assert_eq!(
        a.board(&["ls", "chores"]).ok(),
        "## Home\nc1 Rotate the key. Check: the query below returns nothing\n"
    );
    // Replace: in place of the body, or of none; empty removes it.
    assert_eq!(
        a.board_with_input(&["amend", "b1", "--replace"], b"why: battery")
            .ok(),
        "amended b1\n"
    );
    assert_eq!(
        a.board(&["show", "b1"]).ok(),
        "b1 Dark mode\n  why: battery\n"
    );
    a.board_with_input(&["amend", "b1", "--replace"], b"").ok();
    assert_eq!(a.board(&["show", "b1"]).ok(), "b1 Dark mode\n");
    a.board_with_input(&["amend", "b1", "--replace"], b"back")
        .ok();
    assert_eq!(a.board(&["show", "b1"]).ok(), "b1 Dark mode\n  back\n");
    assert_eq!(
        world.remote_log()[..5],
        [
            "proj: amended b1",
            "proj: amended b1",
            "proj: amended b1",
            "proj: amended c1",
            "proj: add b1"
        ]
    );
    assert_eq!(a.board(&["done", "c1"]).ok(), "done c1\n");
    assert_eq!(text(world.remote_file("proj/chores.md")), "## Home\n");
    in_step(&world, &a);
}

#[test]
fn a_body_command_that_says_nothing_or_names_no_item_is_refused() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "backlog", "Dark mode"]).ok();
    for (args, input, message) in [
        (
            &["amend", "b1"][..],
            &b" \n\n"[..],
            "board: amend b1: no text on stdin, which is where it is read from\n",
        ),
        (
            &["add", "backlog", "x", "--body"][..],
            &b"\n"[..],
            "board: add backlog: no text on stdin, which is where it is read from\n",
        ),
        (
            &["amend", "1", "--replace"][..],
            &b"x"[..],
            "board: amend 1: --replace is for a chore's or backlog item's body; a brief's amendments are a record of what was decided when\n",
        ),
    ] {
        assert_eq!(
            a.board_with_input(args, input).failed(2),
            message,
            "{args:?}"
        );
    }
    assert_eq!(
        a.board_with_input(&["amend", "b9"], b"x").failed(1),
        "board: amend b9: no such backlog item\n"
    );
    assert_eq!(
        text(world.remote_file("proj/backlog.md")),
        "- b1 Dark mode\n"
    );
}

/// A command refused on its arguments exits before it reads stdin: the
/// input outgrows the pipe, so writing it meets a closed pipe every time,
/// and the test still judges the command by what it printed.
#[test]
fn a_command_refused_before_it_reads_stdin_is_judged_by_its_output() {
    let world = World::new();
    let a = world.a();
    let input = vec![b'x'; 1 << 20];
    assert_eq!(
        a.board_with_input(&["amend", "1", "--replace"], &input)
            .failed(2),
        "board: amend 1: --replace is for a chore's or backlog item's body; a brief's \
         amendments are a record of what was decided when\n"
    );
}

/// One clone replaces an item's body while the other deletes the item:
/// after sync both have neither.
#[test]
fn a_body_replaced_on_one_clone_goes_with_its_item_deleted_on_the_other() {
    let world = World::new();
    let (a, b) = (world.a(), world.b());
    a.board_with_input(&["add", "backlog", "Dark mode", "--body"], b"old")
        .ok();
    a.board(&["add", "backlog", "Later"]).ok();
    b.board(&["sync"]).ok();
    b.set_online(false);
    // Offline, so the replace waits on the clone while A drops the item.
    let ran = b.board_with_input(&["amend", "b1", "--replace"], b"new\nand more");
    assert_eq!((ran.code, ran.stdout.as_slice()), (0, &b"amended b1\n"[..]));
    assert_eq!(a.board(&["drop", "b1"]).ok(), "dropped b1\n");
    b.set_online(true);
    assert_eq!(b.board(&["sync"]).code, 0);
    a.board(&["sync"]).ok();
    for file in [
        world.remote_file("proj/backlog.md"),
        a.file("proj/backlog.md"),
        b.file("proj/backlog.md"),
    ] {
        assert_eq!(text(file), "- b2 Later\n");
    }
    in_step(&world, &a);
    in_step(&world, &b);
}

/// An item's indented lines are its own: dropped on one clone while the
/// other edits them, they are gone from both, and the item above keeps
/// only its own.
#[test]
fn an_item_dropped_on_one_clone_goes_with_the_indented_lines_the_other_edited() {
    let world = World::new();
    let (a, b) = (world.a(), world.b());
    a.board(&["add", "backlog", "first"]).ok();
    a.board(&["add", "backlog", "second"]).ok();
    a.write(
        "proj/backlog.md",
        "- b1 first\n  about first\n- b2 second\n  why: slow\n  - and a list\n",
    );
    a.board(&["sync"]).ok();
    b.board(&["sync"]).ok();
    // B edits b2's lines by hand; A drops b2 and pushes first.
    b.write(
        "proj/backlog.md",
        "- b1 first\n  about first\n- b2 second\n  why: very slow\n  - and a list\n  - more\n",
    );
    assert_eq!(a.board(&["drop", "b2"]).ok(), "dropped b2\n");
    let ran = b.board(&["sync"]);
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (
            0,
            "board: merged 1 local commit (by line: proj/backlog.md)\n"
        )
    );
    a.board(&["sync"]).ok();
    let want = "- b1 first\n  about first\n";
    assert_eq!(text(world.remote_file("proj/backlog.md")), want);
    assert_eq!(text(a.file("proj/backlog.md")), want);
    assert_eq!(text(b.file("proj/backlog.md")), want);
    in_step(&world, &a);
    in_step(&world, &b);
}

#[test]
fn move_puts_an_item_next_to_another_in_one_commit() {
    let world = World::new();
    let a = world.a();
    for item in ["one", "two", "three"] {
        a.board(&["add", "backlog", item]).ok();
    }
    for (chore, group) in [("x", "Home"), ("y", "Home"), ("z", "Work")] {
        a.board(&["add", "chore", chore, "--group", group, "--check", "seen"])
            .ok();
    }
    assert_eq!(
        a.board(&["move", "b3", "--before", "b1"]).ok(),
        "moved b3 before b1\n"
    );
    assert_eq!(
        text(world.remote_file("proj/backlog.md")),
        "- b3 three\n- b1 one\n- b2 two\n"
    );
    // A chore goes into the group of the item it is put next to.
    assert_eq!(
        a.board(&["move", "c1", "--after", "c3"]).ok(),
        "moved c1 after c3\n"
    );
    assert_eq!(
        text(world.remote_file("proj/chores.md")),
        "## Home\n- c2 y. Check: seen\n\n## Work\n- c3 z. Check: seen\n- c1 x. Check: seen\n"
    );
    assert_eq!(world.remote_log()[..2], ["proj: move c1", "proj: move b3"]);
    in_step(&world, &a);

    // Where it is already: said so, and no commit.
    let log = world.remote_log();
    assert_eq!(
        a.board(&["move", "b1", "--after", "b3"]).ok(),
        "moved b1 after b3\n"
    );
    assert_eq!(world.remote_log(), log);
    in_step(&world, &a);
}

#[test]
fn move_refuses_ids_that_name_no_single_item() {
    let world = World::new();
    let a = world.a();
    a.write("proj/backlog.md", "- b1 one\n- b2 two\n- b5 x\n- b5 y\n");
    a.commit_and_push("by hand");
    let log = world.remote_log();
    assert_eq!(
        a.board(&["move", "b9", "--before", "b1"]).failed(1),
        "board: lint: proj/backlog.md:4: duplicate id b5, first on line 3; delete the id on one \
         of the lines and that line gets a new one\n\
         board: move b9: no such backlog item\n"
    );
    let target = a.board(&["move", "b1", "--after", "b9"]).failed(1);
    assert!(
        target.ends_with("board: move b1: after b9: no such backlog item\n"),
        "{target}"
    );
    let twice = a.board(&["move", "b1", "--before", "b5"]).failed(1);
    assert!(
        twice.ends_with("board: move b1: before b5: b5 is on 2 lines; fix the duplicate first\n"),
        "{twice}"
    );
    assert_eq!(world.remote_log(), log);
}

/// Wrong whatever the files hold: told before the board is read.
#[test]
fn a_move_the_command_line_rules_out_is_a_usage_error() {
    let world = World::new();
    let a = world.a();
    assert_eq!(
        a.board(&["move", "c1", "--before", "b1"]).failed(2),
        "board: move c1: c1 is a chore and b1 a backlog item; an item moves within its own list\n"
    );
    assert_eq!(
        a.board(&["move", "b1", "--after", "b1"]).failed(2),
        "board: move b1: b1 cannot go next to itself\n"
    );
    let brief = a.board(&["move", "128", "--before", "b1"]).failed(2);
    assert!(
        brief.starts_with(
            "board: invalid value '128' for '<ID>': `128` is not a chore or backlog id like c12 or b41"
        ),
        "{brief}"
    );
    for args in [
        &["move", "b1"][..],
        &["move", "b1", "--before", "b2", "--after", "b3"],
        &["move", "--before", "b2"],
        &["move", "b1", "--after", "c012"],
    ] {
        let said = a.board(args).failed(2);
        assert!(said.starts_with("board: "), "{args:?}: {said}");
    }
    assert_eq!(world.remote_log(), ["start"]);
}

/// A move allocates no id: offline it is committed on the clone, and the
/// next write that reaches the remote pushes it.
#[test]
fn offline_a_move_waits_on_the_clone() {
    let world = World::new();
    let a = world.a();
    for item in ["one", "two"] {
        a.board(&["add", "backlog", item]).ok();
    }
    a.set_online(false);
    let ran = a.board(&["move", "b2", "--before", "b1"]);
    assert_eq!(
        (ran.code, ran.stdout.as_slice()),
        (0, &b"moved b2 before b1\n"[..])
    );
    assert!(
        ran.stderr
            .starts_with("board: not pushed: 1 commit (fatal: "),
        "{ran:?}"
    );
    assert_eq!(text(a.file("proj/backlog.md")), "- b2 two\n- b1 one\n");
    assert_eq!(world.remote_log()[0], "proj: add b2");
    a.set_online(true);
    a.board(&["sync"]).ok();
    assert_eq!(world.remote_log()[0], "proj: move b2");
    assert_eq!(
        text(world.remote_file("proj/backlog.md")),
        "- b2 two\n- b1 one\n"
    );
    in_step(&world, &a);
}

#[test]
fn an_id_on_two_lines_is_refused() {
    let world = World::new();
    let a = world.a();
    a.write(
        "proj/chores.md",
        "- c20 one. Check: x\n- c20 two. Check: x\n",
    );
    a.commit_and_push("hand-typed duplicate");
    // A refused write lints too: the finding says how to fix it.
    assert_eq!(
        a.board(&["done", "c20"]).failed(1),
        "board: lint: proj/chores.md:2: duplicate id c20, first on line 1; delete the id on one \
         of the lines and that line gets a new one\n\
         board: done c20: c20 is on 2 lines; fix the duplicate first\n"
    );
}

#[test]
fn what_is_no_command_line_for_a_write_is_a_usage_error() {
    let world = World::new();
    let a = world.a();
    for args in [
        &["done", "b41"][..],
        &["drop", "c12"],
        &["done"],
        &["add", "chore", "text", "--group", "Home"],
        &["add", "chore", "text", "--group", " ", "--check", "x"],
        &["add", "backlog", ""],
        &["add", "backlog", "two\nlines"],
        &["add", "backlog", "x", "--from", "c012"],
    ] {
        let said = a.board(args).failed(2);
        assert!(said.starts_with("board: "), "{args:?}: {said}");
    }
    assert_eq!(world.remote_log(), ["start"]);
}

#[test]
fn a_chore_whose_check_says_nothing_is_refused() {
    let world = World::new();
    let a = world.a();
    assert_eq!(
        a.board(&[
            "add", "chore", "text", "--group", "Home", "--check", "(126)"
        ])
        .failed(1),
        "board: add chore: `(126)` does not say how to see that the chore is done\n"
    );
    assert_eq!(world.remote_log(), ["start"]);
}

#[test]
fn lint_findings_after_a_write_are_notes() {
    let world = World::new();
    let a = world.a();
    a.write("proj/chores.md", "- c1 no check\n");
    a.commit_and_push("by hand");
    let ran = a.board(&["add", "backlog", "x"]);
    assert_eq!(
        (ran.code, ran.stdout.as_slice(), ran.stderr.as_str()),
        (
            0,
            &b"b1\n"[..],
            "board: lint: proj/chores.md:1: chore without a check; say after `Check:` how to see that it is done\n"
        )
    );
}

#[test]
fn sync_brings_the_clone_to_the_remote() {
    let world = World::new();
    let (a, b) = (world.a(), world.b());
    a.board(&["add", "backlog", "from a"]).ok();
    assert_eq!(b.file("proj/backlog.md"), None);
    assert_eq!(b.board(&["sync"]).ok(), "synced\n");
    assert_eq!(text(b.file("proj/backlog.md")), "- b1 from a\n");
    in_step(&world, &b);
    assert_eq!(world.remote_log(), ["proj: add b1", "start"]);
}

/// `board add backlog x | head -0`: the id is published, so the command
/// succeeded, read or not.
#[test]
fn a_closed_stdout_after_a_write_is_a_quiet_success() {
    let world = World::new();
    let a = world.a();
    let (reader, writer) = io::pipe().unwrap();
    drop(reader);
    let ran = run(a.command().args(["add", "backlog", "x"]).stdout(writer));
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));
    assert_eq!(on_remote(&world, "proj/backlog.md", "- b1 x"), 1);
}

// ---- the next id ----

#[test]
fn an_id_is_never_given_twice_even_after_its_line_is_gone() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "backlog", "one"]).ok();
    a.board(&["add", "backlog", "two"]).ok();
    a.board(&["drop", "b2"]).ok();
    assert_eq!(a.board(&["add", "backlog", "three"]).ok(), "b3\n");
    a.board(&["add", "chore", "one", "--group", "g", "--check", "x"])
        .ok();
    a.board(&["done", "c1"]).ok();
    assert_eq!(
        a.board(&["add", "chore", "two", "--group", "g", "--check", "x"])
            .ok(),
        "c2\n"
    );
}

#[test]
fn a_hand_typed_id_raises_the_next_one() {
    let world = World::new();
    let a = world.a();
    a.write("proj/backlog.md", "- b40 typed by hand\n");
    a.commit_and_push("by hand");
    assert_eq!(a.board(&["add", "backlog", "x"]).ok(), "b41\n");
}

/// History simplification would drop the side branch of a merge whose
/// result matches the main line: an id added and deleted there is in no
/// commit that plain `git log -- <file>` shows.
#[test]
fn an_id_added_and_deleted_on_a_merged_side_branch_is_used() {
    let world = World::new();
    let b = world.b();
    let data = b.data();
    b.write("proj/backlog.md", "- b1 one\n");
    b.commit_and_push("one");
    b.git(&data, ["switch", "-q", "-c", "side"]);
    b.write("proj/backlog.md", "- b1 one\n- b7 on the side\n");
    b.git(&data, ["commit", "-q", "-am", "side add"]);
    b.write("proj/backlog.md", "- b1 one\n");
    b.git(&data, ["commit", "-q", "-am", "side delete"]);
    b.git(&data, ["switch", "-q", "main"]);
    b.git(
        &data,
        ["merge", "-q", "--no-ff", "-m", "merge side", "side"],
    );
    b.git(&data, ["push", "-q", "origin", "main"]);
    assert_eq!(world.a().board(&["add", "backlog", "x"]).ok(), "b8\n");
}

/// A project's directory renamed by hand keeps its ids used: the history
/// is followed across the rename.
#[test]
fn ids_used_before_a_rename_of_the_project_directory_stay_used() {
    let world = World::new();
    let b = world.b();
    b.write("old/chores.md", "- c1 one. Check: x\n- c5 five. Check: x\n");
    b.commit_and_push("old");
    b.write("old/chores.md", "- c1 one. Check: x\n");
    b.commit_and_push("c5 done");
    b.git(&b.data(), ["mv", "old", "proj"]);
    b.commit_and_push("rename");
    assert_eq!(
        world
            .a()
            .board(&["add", "chore", "x", "--group", "g", "--check", "x"])
            .ok(),
        "c6\n"
    );
}

/// A project name used again, by renaming another directory onto it: the
/// ids the name's earlier file held stay used. At the rename `--follow`
/// goes on with the source path only.
#[test]
fn ids_of_a_removed_project_stay_used_when_a_directory_is_renamed_onto_its_name() {
    let world = World::new();
    let b = world.b();
    b.write("proj/backlog.md", "- b1 one\n- b9 nine\n");
    b.commit_and_push("proj");
    b.git(&b.data(), ["rm", "-q", "-r", "proj"]);
    b.commit_and_push("proj removed");
    b.write("other/backlog.md", "- b1 one\n- b3 three\n");
    b.commit_and_push("other");
    b.git(&b.data(), ["mv", "other", "proj"]);
    b.commit_and_push("rename");
    assert_eq!(world.a().board(&["add", "backlog", "x"]).ok(), "b10\n");
}

#[test]
fn the_last_id_ends_allocation_for_its_list() {
    let world = World::new();
    let a = world.a();
    a.write("proj/backlog.md", "- b4294967295 the last\n");
    a.commit_and_push("by hand");
    assert_eq!(
        a.board(&["add", "backlog", "x"]).failed(1),
        "board: add backlog: b4294967295 has been used, the highest id there is; no id is left\n"
    );
    assert_eq!(
        a.board(&["add", "chore", "x", "--group", "g", "--check", "x"])
            .ok(),
        "c1\n"
    );
}

// ---- pushes that fail ----

#[test]
fn a_refused_push_is_retried_on_fresh_state() {
    let world = World::new();
    let a = world.a();
    world.reject_pushes(1);
    let ran = a.board(&["add", "backlog", "x"]);
    assert_eq!(
        (ran.code, ran.stdout.as_slice(), ran.stderr.as_str()),
        (0, &b"b1\n"[..], "board: retry 2/4\n")
    );
    assert_eq!(on_remote(&world, "proj/backlog.md", "x"), 1);
    in_step(&world, &a);
}

/// Four refused pushes count as offline: an id is refused, since only a
/// push may publish it, and any other write is committed on the clone and
/// pushed by the next write that gets through.
#[test]
fn four_refused_pushes_refuse_an_id_and_keep_other_writes_on_the_clone() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "chore", "x", "--group", "g", "--check", "x"])
        .ok();
    let before = world.remote_main();
    world.reject_pushes(9);
    let retries = "board: retry 2/4\nboard: retry 3/4\nboard: retry 4/4\n";
    let refused = " -> main (pre-receive hook declined))\n";

    let said = a.board(&["add", "backlog", "x"]).failed(3);
    assert!(
        said.starts_with(&format!(
            "{retries}board: add backlog: not pushed after 4 attempts (! [remote rejected] "
        )),
        "{said}"
    );
    assert!(said.ends_with(refused), "{said}");
    assert_eq!(a.status(), "");
    assert_eq!(a.main(), before);

    let ran = a.board(&["done", "c1"]);
    assert_eq!((ran.code, ran.stdout.as_slice()), (0, &b"done c1\n"[..]));
    assert!(
        ran.stderr.starts_with(&format!(
            "{retries}board: not pushed: 1 commit (! [remote rejected] "
        )),
        "{ran:?}"
    );
    assert!(ran.stderr.ends_with(refused), "{ran:?}");
    assert_eq!(world.remote_main(), before);
    assert_eq!(text(a.file("proj/chores.md")), "## g\n");

    world.reject_pushes(0);
    assert_eq!(a.board(&["add", "backlog", "y"]).ok(), "b1\n");
    assert_eq!(text(world.remote_file("proj/chores.md")), "## g\n");
    assert_eq!(world.remote_log()[..2], ["proj: add b1", "proj: done c1"]);
    in_step(&world, &a);
}

/// B writes while A's push is on its way: A's push is refused, and A runs
/// its command again on what B pushed.
#[test]
fn two_clones_writing_at_once_get_different_ids() {
    let world = World::new();
    let (a, b) = (world.a(), world.b());
    a.hook_once(
        "pre-push",
        &pre_push::window(&b.script(&["add", "backlog", "from b"]), world.dir()),
    );
    let ran = a.board(&["add", "backlog", "from a"]);
    let window = |name: &str| fs::read_to_string(world.dir().join(name)).unwrap();
    assert_eq!(
        (
            window("window.code"),
            window("window.out"),
            window("window.err")
        ),
        ("0\n".to_owned(), "b1\n".to_owned(), String::new())
    );
    assert_eq!(
        (ran.code, ran.stdout.as_slice(), ran.stderr.as_str()),
        (0, &b"b2\n"[..], "board: retry 2/4\n")
    );
    assert_eq!(
        text(world.remote_file("proj/backlog.md")),
        "- b1 from b\n- b2 from a\n"
    );
    in_step(&world, &a);
}

/// The push reached the remote, but `board` heard that it failed: the
/// command is not run again, and its first result stands.
#[test]
fn a_push_that_landed_unheard_is_not_applied_twice() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "chore", "x", "--group", "g", "--check", "x"])
        .ok();

    a.hook_once("pre-push", pre_push::LOST_ACK);
    let ran = a.board(&["add", "backlog", "once"]);
    assert_eq!(
        (ran.code, ran.stdout.as_slice(), ran.stderr.as_str()),
        (0, &b"b1\n"[..], "board: retry 2/4\n")
    );
    assert_eq!(on_remote(&world, "proj/backlog.md", "once"), 1);

    // Run again, `done` would find c1 gone and refuse.
    a.hook_once("pre-push", pre_push::LOST_ACK);
    let ran = a.board(&["done", "c1"]);
    assert_eq!(
        (ran.code, ran.stdout.as_slice(), ran.stderr.as_str()),
        (0, &b"done c1\n"[..], "board: retry 2/4\n")
    );
    assert_eq!(
        world.remote_log()[..3],
        ["proj: done c1", "proj: add b1", "proj: add c1"]
    );
    in_step(&world, &a);
}

/// The first push is given up on but lands after the second round's fetch:
/// the second round's push is refused, and the third finds the first's
/// commit on the remote rather than run the command once more.
#[test]
fn a_push_that_landed_late_is_not_applied_again() {
    let world = World::new();
    let a = world.a();
    a.hook("pre-push", &pre_push::late_landing(world.dir()));
    let ran = a.board(&["add", "backlog", "once"]);
    assert_eq!(
        (ran.code, ran.stdout.as_slice(), ran.stderr.as_str()),
        (0, &b"b1\n"[..], "board: retry 2/4\nboard: retry 3/4\n")
    );
    assert_eq!(text(world.remote_file("proj/backlog.md")), "- b1 once\n");
    assert_eq!(world.remote_log(), ["proj: add b1", "start"]);
    in_step(&world, &a);
}

#[test]
fn a_command_killed_before_its_push_changed_nothing() {
    let world = World::new();
    let a = world.a();
    let before = world.remote_main();
    a.hook_once("pre-push", pre_push::KILL_BEFORE_PUSH);
    a.board_killed(&["add", "backlog", "lost"]);
    assert_eq!(world.remote_main(), before);
    assert_eq!(a.file("proj/backlog.md"), None);
    // The lock died with it.
    assert_eq!(a.board(&["add", "backlog", "next"]).ok(), "b1\n");
    assert_eq!(text(world.remote_file("proj/backlog.md")), "- b1 next\n");
    in_step(&world, &a);
}

/// Killed between the push and the update of its own clone: the clone is
/// behind the remote, and the next command catches up without applying
/// anything twice.
#[test]
fn a_command_killed_after_its_push_is_healed_by_the_next() {
    let world = World::new();
    let a = world.a();
    a.hook_once("pre-push", pre_push::KILL_AFTER_PUSH);
    a.board_killed(&["add", "backlog", "landed"]);
    assert_eq!(text(world.remote_file("proj/backlog.md")), "- b1 landed\n");
    assert_eq!(a.file("proj/backlog.md"), None);
    assert_eq!(a.board(&["add", "backlog", "next"]).ok(), "b2\n");
    assert_eq!(text(a.file("proj/backlog.md")), "- b1 landed\n- b2 next\n");
    in_step(&world, &a);
}

/// Offline, a write that allocates an id is refused (exit 3) and any other
/// write is committed on the clone, hand edits too; the next write that
/// reaches the remote pushes them.
#[test]
fn offline_an_id_is_refused_and_other_writes_wait_on_the_clone() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "chore", "x", "--group", "g", "--check", "x"])
        .ok();
    let before = world.remote_main();
    a.set_online(false);
    let unreached = "needs the remote, which was not reached (fatal: ";

    let said = a.board(&["add", "backlog", "x"]).failed(3);
    assert!(
        said.starts_with(&format!("board: add backlog: {unreached}")),
        "{said}"
    );
    assert_eq!(said.lines().count(), 1, "{said}");

    let ran = a.board(&["done", "c1"]);
    assert_eq!((ran.code, ran.stdout.as_slice()), (0, &b"done c1\n"[..]));
    assert!(
        ran.stderr
            .starts_with("board: not pushed: 1 commit (fatal: "),
        "{ran:?}"
    );
    assert_eq!(ran.stderr.lines().count(), 1, "{ran:?}");

    // A hand edit: `sync` commits it, and says it could not push.
    a.write("proj/backlog.md", "- by hand\n");
    let said = a.board(&["sync"]).failed(3);
    let lines: Vec<&str> = said.lines().collect();
    assert_eq!(lines.len(), 2, "{said}");
    assert!(
        lines[0].starts_with("board: not pushed: 2 commits (fatal: "),
        "{said}"
    );
    assert!(
        lines[1].starts_with(&format!("board: sync: {unreached}")),
        "{said}"
    );
    assert_eq!(a.status(), "");
    assert_eq!(world.remote_main(), before);
    // The item without an id waits for its id until the remote is there.
    assert_eq!(text(a.file("proj/backlog.md")), "- by hand\n");

    a.set_online(true);
    let ran = a.board(&["add", "backlog", "next"]);
    assert_eq!(
        (ran.code, ran.stdout.as_slice(), ran.stderr.as_str()),
        (0, &b"b2\n"[..], "board: assigned b1: by hand\n")
    );
    assert_eq!(text(world.remote_file("proj/chores.md")), "## g\n");
    assert_eq!(
        text(world.remote_file("proj/backlog.md")),
        "- b1 by hand\n- b2 next\n"
    );
    assert_eq!(
        world.remote_log()[..4],
        [
            "proj: add b2",
            "proj: assign b1",
            "board: hand edits",
            "proj: done c1"
        ]
    );
    in_step(&world, &a);
}

// ---- the clone ----

/// Two writes on one clone: the second waits for the first, says so before
/// it waits, and both land once.
///
/// The second is started by the first's `pre-push` hook, so it runs exactly
/// while the first holds the lock. Its stderr is a FIFO: the hook reads its
/// first line (which `board` writes before it blocks), leaves a `cat`
/// draining the rest and lets the push go on. Every stream of what the hook
/// starts is redirected, or the first `board` would wait for their end.
#[test]
fn a_second_write_on_a_clone_waits_for_the_first() {
    let world = World::new();
    let a = world.a();
    let file = |name: &str| quote(&world.dir().join(name).to_string_lossy());
    a.hook_once(
        "pre-push",
        &format!(
            "mkfifo {err} {code}\n\
             ({second} </dev/null >{out} 2>{err}; echo $? >{code}) </dev/null >/dev/null 2>&1 &\n\
             exec 3<{err}\n\
             IFS= read -r line <&3\n\
             printf '%s\\n' \"$line\" >{first}\n\
             cat <&3 >{rest} 2>/dev/null &\n\
             exit 0\n",
            second = a.script(&["add", "backlog", "second"]),
            out = file("second.out"),
            err = file("second.err"),
            code = file("second.code"),
            first = file("first-line"),
            rest = file("rest"),
        ),
    );
    assert_eq!(a.board(&["add", "backlog", "first"]).ok(), "b1\n");
    let read = |name: &str| fs::read_to_string(world.dir().join(name)).unwrap();
    // Blocks until the second has ended.
    assert_eq!(read("second.code"), "0\n");
    assert_eq!(read("first-line"), "board: waiting for lock\n");
    assert_eq!(read("second.out"), "b2\n");
    assert_eq!(
        text(world.remote_file("proj/backlog.md")),
        "- b1 first\n- b2 second\n"
    );
    in_step(&world, &a);
}

/// git's locks on the clone's index and the tool's, left by a command
/// killed while git wrote one: the next write goes on, and commits a hand
/// edit through the clone's index.
#[test]
fn stale_locks_on_the_indexes_are_removed() {
    let world = World::new();
    let a = world.a();
    let locks = ["index.lock", "board.index.lock"].map(|name| a.data().join(".git").join(name));
    for lock in &locks {
        fs::write(lock, "").unwrap();
    }
    a.write("proj/chores.md", "- c1 by hand. Check: x\n");
    assert_eq!(a.board(&["add", "backlog", "x"]).ok(), "b1\n");
    for lock in &locks {
        assert!(!lock.exists(), "{}", lock.display());
    }
    assert_eq!(
        world.remote_log()[..2],
        ["proj: add b1", "board: hand edits"]
    );
    in_step(&world, &a);
}

/// Configuration a user may have that would change what the tool reads or
/// writes: colour codes, a textconv driver, a signature check and a file
/// taken for binary in the history it parses, line ending conversion,
/// merges that refuse to fast-forward, signing without a key, untracked
/// files hidden, signed pushes and verified merges. Each fault that reaches
/// the history would raise the next id past the one asserted.
#[test]
fn a_hostile_git_configuration_changes_nothing() {
    let world = World::new();
    let a = world.a();
    let script = |name: &str, body: &str| {
        let path = world.dir().join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        path.to_str().unwrap().to_owned()
    };
    // Makes a signature git takes, to sign a commit by hand. Each fake reads
    // what git writes it first: one that exits before git is done writing
    // breaks git's pipe, and git then fails now and then, under load.
    let signer = script(
        "signer",
        "cat >/dev/null\n\
         echo '[GNUPG:] SIG_CREATED ' >&2\n\
         printf -- '-----BEGIN PGP SIGNATURE-----\\n\\nfake\\n-----END PGP SIGNATURE-----\\n'\n",
    );
    // Signs nothing, and says an id line when asked to check a signature.
    let gpg = script(
        "gpg",
        "cat >/dev/null\necho '+- b99 not a line' >&2\nexit 1\n",
    );
    let textconv = script("textconv", "cat \"$1\"\necho '- c99 not a line'\n");
    let attributes = world.dir().join("attributes");
    fs::write(
        &attributes,
        "proj/backlog.md -diff\nproj/chores.md diff=garbage\n",
    )
    .unwrap();

    a.board(&["add", "backlog", "one"]).ok();
    a.board(&["add", "backlog", "two"]).ok();
    a.write("proj/backlog.md", "- b1 one\n- b2 two\n- b5 signed\n");
    a.git(
        &a.data(),
        [
            "-c",
            &format!("gpg.program={signer}"),
            "commit",
            "-q",
            "-S",
            "-am",
            "signed by hand",
        ],
    );
    a.git(&a.data(), ["push", "-q", "origin", "main"]);
    a.board(&["drop", "b5", "b2"]).ok();
    a.board(&["add", "chore", "one", "--group", "g", "--check", "x"])
        .ok();
    a.board(&["done", "c1"]).ok();
    for (key, value) in [
        ("color.ui", "always"),
        ("color.diff", "always"),
        ("core.attributesFile", attributes.to_str().unwrap()),
        ("diff.garbage.textconv", &textconv),
        ("core.autocrlf", "true"),
        ("merge.ff", "false"),
        ("commit.gpgsign", "true"),
        ("gpg.program", &gpg),
        ("log.showSignature", "true"),
        ("status.showUntrackedFiles", "no"),
        ("push.gpgSign", "true"),
        ("merge.verifySignatures", "true"),
        ("diff.noprefix", "true"),
        ("diff.mnemonicPrefix", "true"),
    ] {
        a.git(&a.data(), ["config", key, value]);
    }
    assert_eq!(a.board(&["add", "backlog", "three"]).ok(), "b6\n");
    assert_eq!(
        a.board(&["add", "chore", "two", "--group", "g", "--check", "x"])
            .ok(),
        "c2\n"
    );
    a.board(&["drop", "b1"]).ok();
    assert_eq!(text(world.remote_file("proj/backlog.md")), "- b6 three\n");
    assert_eq!(text(a.file("proj/backlog.md")), "- b6 three\n");
    in_step(&world, &a);
    // An untracked board file is seen with untracked files hidden; the
    // commit of hand edits, the merge and its line merge go by the tool's
    // settings, not these; hooks that refuse every commit do not run.
    for (key, value) in [
        ("merge.conflictStyle", "zdiff3"),
        ("diff.algorithm", "patience"),
        ("status.renames", "copies"),
        ("commit.verbose", "true"),
        ("commit.cleanup", "verbatim"),
    ] {
        a.git(&a.data(), ["config", key, value]);
    }
    a.hook("pre-commit", "exit 1\n");
    a.hook("commit-msg", "exit 1\n");
    world.b().board(&["add", "backlog", "from b"]).ok();
    a.write("shop/chores.md", "- hand. Check: x\n");
    a.write("proj/backlog.md", "- b6 three, edited\n");
    let ran = a.board(&["add", "backlog", "four"]);
    assert_eq!(
        (ran.code, ran.stdout.as_slice(), ran.stderr.as_str()),
        (
            0,
            &b"b8\n"[..],
            "board: merged 1 local commit (by line: proj/backlog.md)\n\
             board: assigned c1: hand. Check: x\n"
        )
    );
    assert_eq!(
        text(world.remote_file("proj/backlog.md")),
        "- b7 from b\n- b6 three, edited\n- b8 four\n"
    );
    assert_eq!(
        text(world.remote_file("shop/chores.md")),
        "- c1 hand. Check: x\n"
    );
    in_step(&world, &a);
}

// ---- hand edits and merges ----

/// Every write commits the board files changed by hand first, and every
/// write that reaches the remote gives an item without an id its id, in
/// every project. What is not the board's stays untracked.
#[test]
fn hand_edits_are_committed_and_an_item_without_an_id_gets_one() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "chore", "one", "--group", "Home", "--check", "x"])
        .ok();
    a.write(
        "proj/chores.md",
        "## Home\n- c1 one, edited. Check: x\n- by hand. Check: y\n",
    );
    a.write("shop/backlog.md", "- elsewhere\n- b7 typed\n");
    a.write("README.md", "notes\n");
    a.write("proj/briefs/128-x.md.local", "a copy\n");
    let ran = a.board(&["add", "backlog", "x"]);
    assert_eq!(
        (ran.code, ran.stdout.as_slice()),
        (0, &b"b1\n"[..]),
        "{ran:?}"
    );
    assert_eq!(
        ran.stderr,
        "board: assigned c2: by hand. Check: y\n\
         board: assigned b8: elsewhere\n\
         board: lint: proj/briefs/128-x.md.local: unmerged local version of brief 128; \
         merge it by hand, then delete this file\n"
    );
    assert_eq!(
        text(world.remote_file("proj/chores.md")),
        "## Home\n- c1 one, edited. Check: x\n- c2 by hand. Check: y\n"
    );
    assert_eq!(
        text(world.remote_file("shop/backlog.md")),
        "- b8 elsewhere\n- b7 typed\n"
    );
    assert_eq!(
        world.remote_log()[..4],
        [
            "proj: add b1",
            "shop: assign b8",
            "proj: assign c2",
            "board: hand edits"
        ]
    );
    assert_eq!(world.remote_file("README.md"), None);
    assert_eq!(a.main(), world.remote_main());
    assert_eq!(a.status(), "?? README.md\n?? proj/briefs/128-x.md.local\n");
}

/// Both clones changed the backlog: the clone's commits are merged with the
/// remote's by a merge commit, the list line by line, keyed by id. A delete
/// beats an edit and a move, adds of both sides stay, the local edit wins.
#[test]
fn diverged_clones_are_merged_by_id() {
    let world = World::new();
    let (a, b) = (world.a(), world.b());
    for item in ["one", "two", "three", "four"] {
        a.board(&["add", "backlog", item]).ok();
    }
    a.write("proj/briefs/1-x.md", "id: 1\nstatus: draft\n");
    a.board(&["sync"]).ok();
    b.board(&["sync"]).ok();
    // The remote: b1 dropped, b2 edited, b3 moved to the top, b5 added.
    b.write(
        "proj/backlog.md",
        "- b3 three\n- b2 two, remote\n- b4 four\n",
    );
    assert_eq!(b.board(&["add", "backlog", "five"]).ok(), "b5\n");
    // The clone: b1 edited, b3 dropped, b4 edited, an item without id, and
    // the brief deleted.
    a.write(
        "proj/backlog.md",
        "- b1 one, local\n- b2 two\n- b4 four, local\n- six\n",
    );
    fs::remove_file(a.data().join("proj/briefs/1-x.md")).unwrap();
    let ran = a.board(&["add", "backlog", "seven"]);
    assert_eq!(
        (ran.code, ran.stdout.as_slice(), ran.stderr.as_str()),
        (
            0,
            &b"b7\n"[..],
            "board: merged 1 local commit (by line: proj/backlog.md)\n\
             board: assigned b6: six\n"
        )
    );
    // The line order is the union's: b4 sits where the clone's edit of it
    // was, after the remote's b5. Order is not data.
    assert_eq!(
        text(world.remote_file("proj/backlog.md")),
        "- b2 two, remote\n- b5 five\n- b4 four, local\n- b6 six\n- b7 seven\n"
    );
    assert_eq!(world.remote_file("proj/briefs/1-x.md"), None);
    let log = world.remote_log();
    assert_eq!(
        log[..3],
        ["proj: add b7", "proj: assign b6", "board: merge"]
    );
    assert!(log.contains(&"board: hand edits".to_owned()), "{log:?}");
    in_step(&world, &a);
    b.board(&["sync"]).ok();
    in_step(&world, &b);
}

/// An id typed by hand on both clones for two items: both lines survive the
/// merge (which one `closes b41` meant is a guess), lint says how to fix it,
/// and a command refuses the id until it is fixed.
#[test]
fn a_duplicate_id_survives_a_merge_and_is_refused() {
    let world = World::new();
    let (a, b) = (world.a(), world.b());
    b.write("proj/chores.md", "- c7 from b. Check: x\n");
    b.board(&["sync"]).ok();
    a.write("proj/chores.md", "- c7 from a. Check: x\n");
    let duplicate = "board: lint: proj/chores.md:2: duplicate id c7, first on line 1; delete \
                     the id on one of the lines and that line gets a new one\n";
    let ran = a.board(&["sync"]);
    assert_eq!(
        (ran.code, ran.stderr),
        (
            0,
            format!("board: merged 1 local commit (by line: proj/chores.md)\n{duplicate}")
        )
    );
    assert_eq!(
        text(world.remote_file("proj/chores.md")),
        "- c7 from b. Check: x\n- c7 from a. Check: x\n"
    );
    assert_eq!(
        a.board(&["done", "c7"]).failed(1),
        format!("{duplicate}board: done c7: c7 is on 2 lines; fix the duplicate first\n")
    );
    // Deleting the id on one line gives that line a new one.
    a.write(
        "proj/chores.md",
        "- c7 from b. Check: x\n- from a. Check: x\n",
    );
    let ran = a.board(&["done", "c7"]);
    assert_eq!(
        (ran.code, ran.stdout.as_slice(), ran.stderr.as_str()),
        (
            0,
            &b"done c7\n"[..],
            "board: assigned c8: from a. Check: x\n"
        )
    );
    assert_eq!(
        text(world.remote_file("proj/chores.md")),
        "- c8 from a. Check: x\n"
    );
}

/// A brief both clones changed: the remote's version stays, the clone's is
/// saved beside it, the note says so once and lint says so on every write
/// in that project until the copy is deleted. Writes go on meanwhile.
#[test]
fn a_brief_changed_on_both_clones_keeps_the_remote_s_and_saves_the_local_one() {
    let world = World::new();
    let (a, b) = (world.a(), world.b());
    let brief = "proj/briefs/1-x.md";
    let header = "id: 1\nstatus: draft\n\n# X\n";
    a.write(brief, header);
    a.board(&["sync"]).ok();
    b.board(&["sync"]).ok();
    b.write(brief, format!("{header}from b\n"));
    b.board(&["sync"]).ok();
    a.write(brief, format!("{header}from a\n"));

    let copy = a.data().join("proj/briefs/1-x.md.local");
    let finding = "board: lint: proj/briefs/1-x.md.local: unmerged local version of brief 1; \
                   merge it by hand, then delete this file\n";
    let ran = a.board(&["sync"]);
    assert_eq!(
        (ran.code, ran.stdout.as_slice(), ran.stderr),
        (
            0,
            &b"synced\n"[..],
            format!(
                "board: merged 1 local commit\n\
                 board: conflict: brief 1 changed here and on the remote; the remote version \
                 is kept, yours is saved as {}\n{finding}",
                copy.display()
            )
        )
    );
    assert_eq!(text(a.file(brief)), format!("{header}from b\n"));
    assert_eq!(text(world.remote_file(brief)), format!("{header}from b\n"));
    assert_eq!(
        text(a.file("proj/briefs/1-x.md.local")),
        format!("{header}from a\n")
    );
    assert_eq!(a.main(), world.remote_main());

    // Another project's write knows nothing of it; this project's says it
    // again, and so does lint.
    let ran = a.board(&["--board", "shop", "add", "backlog", "x"]);
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));
    let ran = a.board(&["add", "backlog", "y"]);
    assert_eq!((ran.code, ran.stderr), (0, finding.to_owned()));
    assert_eq!(a.board(&["lint"]).out(1), &finding["board: lint: ".len()..]);
    fs::remove_file(&copy).unwrap();
    assert_eq!(a.board(&["lint"]).ok(), "ok\n");
    assert_eq!(a.board(&["add", "backlog", "z"]).ok(), "b2\n");
}

/// The local version is saved before the merge is pushed: a write killed
/// right after its push leaves the copy, and the next write, which finds
/// the merge on the remote already, has no conflict left to see.
#[test]
fn a_brief_s_local_version_is_saved_before_the_push() {
    let world = World::new();
    let (a, b) = (world.a(), world.b());
    let brief = "proj/briefs/1-x.md";
    let header = "id: 1\nstatus: draft\n\n# X\n";
    b.write(brief, format!("{header}from b\n"));
    b.board(&["sync"]).ok();
    a.write(brief, format!("{header}from a\n"));
    a.hook_once("pre-push", pre_push::KILL_AFTER_PUSH);
    a.board_killed(&["add", "backlog", "x"]);
    assert_eq!(
        text(a.file("proj/briefs/1-x.md.local")),
        format!("{header}from a\n")
    );
    let ran = a.board(&["sync"]);
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (
            0,
            "board: lint: proj/briefs/1-x.md.local: unmerged local version of brief 1; \
             merge it by hand, then delete this file\n"
        )
    );
    assert_eq!(text(a.file(brief)), format!("{header}from b\n"));
    assert_eq!(a.status(), "?? proj/briefs/1-x.md.local\n");
    assert_eq!(a.main(), world.remote_main());
}

/// A project directory renamed by hand and synced: the ids used before the
/// rename stay used.
#[test]
fn a_project_directory_renamed_by_hand_keeps_its_ids_used() {
    let world = World::new();
    let b = world.b();
    b.write("old/chores.md", "- c1 one. Check: x\n- c5 five. Check: x\n");
    b.board(&["sync"]).ok();
    b.write("old/chores.md", "- c1 one. Check: x\n");
    b.board(&["sync"]).ok();
    fs::rename(b.data().join("old"), b.data().join("proj")).unwrap();
    assert_eq!(b.board(&["sync"]).ok(), "synced\n");
    assert_eq!(
        world
            .a()
            .board(&["add", "chore", "x", "--group", "g", "--check", "x"])
            .ok(),
        "c6\n"
    );
}

/// A file saved while the write's push is on its way: the fast-forward
/// refuses to overwrite it, so the save is committed and merged with the
/// write's commit, from the exact base, and the next write pushes it. A save
/// to a file the write did not change goes along with the fast-forward and
/// is committed by the next write.
#[test]
fn a_file_saved_during_the_push_is_merged_in() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "backlog", "one"]).ok();
    // Another file than the write's: the fast-forward carries it.
    a.hook_once(
        "pre-push",
        "printf '%s\\n' '- by hand. Check: x' >proj/chores.md\n",
    );
    assert_eq!(a.board(&["add", "backlog", "two"]).ok(), "b2\n");
    assert_eq!(a.main(), world.remote_main());
    assert_eq!(a.status(), "?? proj/chores.md\n");

    // The write's own file: committed and merged.
    a.hook_once(
        "pre-push",
        "printf '%s\\n' '- b1 one, edited' '- b2 two' >proj/backlog.md\n",
    );
    let ran = a.board(&["add", "backlog", "three"]);
    assert_eq!(
        (ran.code, ran.stdout.as_slice(), ran.stderr.as_str()),
        (
            0,
            &b"b3\n"[..],
            "board: merged 1 local commit (by line: proj/backlog.md)\n\
             board: assigned c1: by hand. Check: x\n"
        )
    );
    assert_eq!(
        text(world.remote_file("proj/backlog.md")),
        "- b1 one\n- b2 two\n- b3 three\n"
    );
    assert_eq!(
        text(a.file("proj/backlog.md")),
        "- b1 one, edited\n- b2 two\n- b3 three\n"
    );
    assert_eq!(a.status(), "");

    assert_eq!(a.board(&["add", "backlog", "four"]).ok(), "b4\n");
    assert_eq!(
        text(world.remote_file("proj/backlog.md")),
        "- b1 one, edited\n- b2 two\n- b3 three\n- b4 four\n"
    );
    assert_eq!(
        text(world.remote_file("proj/chores.md")),
        "- c1 by hand. Check: x\n"
    );
    in_step(&world, &a);
}

/// Killed after its push reached the remote, with the clone's own commits
/// merged into that push: the clone is behind, and the next write catches up
/// without merging the hand edit again, so the line is there once.
#[test]
fn a_command_killed_after_pushing_a_merge_is_healed_without_a_second_merge() {
    let world = World::new();
    let (a, b) = (world.a(), world.b());
    b.board(&["add", "backlog", "from b"]).ok();
    a.write("proj/backlog.md", "- by hand\n");
    a.hook_once("pre-push", pre_push::KILL_AFTER_PUSH);
    a.board_killed(&["add", "backlog", "landed"]);
    assert_eq!(
        text(world.remote_file("proj/backlog.md")),
        "- b1 from b\n- b2 by hand\n- b3 landed\n"
    );
    assert_eq!(a.board(&["add", "backlog", "next"]).ok(), "b4\n");
    assert_eq!(
        text(world.remote_file("proj/backlog.md")),
        "- b1 from b\n- b2 by hand\n- b3 landed\n- b4 next\n"
    );
    in_step(&world, &a);
}

/// A line without an id, published under an id while the clone's file still
/// shows it without one (a save during the push; a kill after it), and then
/// deleted or moved there: the delete and the move stand, read with the id
/// the push gave, even after another clone edited that id.
#[test]
fn a_line_whose_id_was_being_published_is_deleted_and_moved_by_its_id() {
    let world = World::new();
    let (a, b) = (world.a(), world.b());

    // Deleted while the push runs.
    a.write("proj/backlog.md", "- foo\n");
    a.hook_once("pre-push", ": >proj/backlog.md\n");
    let ran = a.board(&["add", "backlog", "x"]);
    assert_eq!(
        (ran.code, ran.stdout.as_slice()),
        (0, &b"b2\n"[..]),
        "{ran:?}"
    );
    assert_eq!(text(a.file("proj/backlog.md")), "- b2 x\n");
    a.board(&["sync"]).ok();
    assert_eq!(text(world.remote_file("proj/backlog.md")), "- b2 x\n");
    in_step(&world, &a);

    // Killed after the push; meanwhile another clone edits the published
    // line; then the clone moves one line and deletes the other.
    a.write("proj/backlog.md", "- b2 x\n- bar\n- baz\n");
    a.hook_once("pre-push", pre_push::KILL_AFTER_PUSH);
    a.board_killed(&["add", "backlog", "y"]);
    assert_eq!(
        text(world.remote_file("proj/backlog.md")),
        "- b2 x\n- b3 bar\n- b4 baz\n- b5 y\n"
    );
    assert_eq!(text(a.file("proj/backlog.md")), "- b2 x\n- bar\n- baz\n");
    b.board(&["sync"]).ok();
    b.write(
        "proj/backlog.md",
        "- b2 x\n- b3 bar, edited\n- b4 baz\n- b5 y\n",
    );
    b.board(&["sync"]).ok();
    a.write("proj/backlog.md", "- baz\n- b2 x\n");
    let ran = a.board(&["sync"]);
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (
            0,
            "board: merged 1 local commit (by line: proj/backlog.md)\n"
        ),
        "{ran:?}"
    );
    // Each line once; where the moved one sits is the union's, and its old
    // neighbour changed on the remote. Order is not data.
    let mut lines: Vec<String> = text(world.remote_file("proj/backlog.md"))
        .lines()
        .map(str::to_owned)
        .collect();
    lines.sort();
    assert_eq!(lines, ["- b2 x", "- b4 baz", "- b5 y"]);
    in_step(&world, &a);
}

/// The residue of the case above: a line without an id whose text is changed
/// while its id is published keeps the change under a new id, and the
/// published id goes. Never two lines.
#[test]
fn a_line_changed_while_its_id_was_being_published_gets_a_new_id() {
    let world = World::new();
    let a = world.a();
    a.write("proj/backlog.md", "- foo\n");
    a.hook_once(
        "pre-push",
        "printf '%s\\n' '- foo, edited' >proj/backlog.md\n",
    );
    let ran = a.board(&["add", "backlog", "x"]);
    assert_eq!(
        (ran.code, ran.stdout.as_slice(), ran.stderr.as_str()),
        (
            0,
            &b"b2\n"[..],
            "board: merged 1 local commit (by line: proj/backlog.md)\n\
             board: assigned b1: foo\n"
        )
    );
    let ran = a.board(&["sync"]);
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (0, "board: assigned b3: foo, edited\n"),
        "{ran:?}"
    );
    assert_eq!(
        text(world.remote_file("proj/backlog.md")),
        "- b2 x\n- b3 foo, edited\n"
    );
    in_step(&world, &a);
}

/// A board file staged as new and then deleted (a write killed between the
/// add and the commit of its hand edits, then the file removed): in no
/// version git knows, so there is nothing to commit for it, and the write
/// goes on.
#[test]
fn a_new_file_staged_and_then_deleted_is_no_hand_edit() {
    let world = World::new();
    let a = world.a();
    a.write("proj/chores.md", "- by hand. Check: x\n");
    a.git(&a.data(), ["add", "proj/chores.md"]);
    fs::remove_file(a.data().join("proj/chores.md")).unwrap();
    a.write("proj/backlog.md", "- b9 typed\n");
    assert_eq!(a.board(&["add", "backlog", "x"]).ok(), "b10\n");
    assert_eq!(world.remote_file("proj/chores.md"), None);
    assert_eq!(
        text(world.remote_file("proj/backlog.md")),
        "- b9 typed\n- b10 x\n"
    );
    in_step(&world, &a);
}

/// Ahead only (a commit by hand, nothing new on the remote): the write
/// pushes it with its own.
#[test]
fn a_clone_ahead_of_the_remote_pushes_its_commits_with_the_write() {
    let world = World::new();
    let a = world.a();
    a.write("shop/backlog.md", "- b1 by hand\n");
    a.git(&a.data(), ["add", "-A"]);
    a.git(&a.data(), ["commit", "-q", "-m", "by hand"]);
    assert_eq!(a.board(&["add", "backlog", "x"]).ok(), "b1\n");
    assert_eq!(world.remote_log(), ["proj: add b1", "by hand", "start"]);
    in_step(&world, &a);
}

#[test]
fn a_clone_the_board_cannot_write_to_is_a_setup_failure() {
    let world = World::new();
    let (a, b) = (world.a(), world.b());
    let not_ready = |machine: &Machine, why: &str| {
        format!(
            "board: {root} is not a board-data clone the board can write to: {why}; \
             clone the board-data repo there once (git clone <url> {root})\n",
            root = machine.data().display()
        )
    };

    b.git(&b.data(), ["switch", "-q", "--detach"]);
    assert_eq!(
        b.board(&["add", "backlog", "x"]).failed(4),
        not_ready(&b, "HEAD is not on main")
    );
    b.git(&b.data(), ["switch", "-q", "main"]);
    b.git(&b.data(), ["remote", "remove", "origin"]);
    assert_eq!(
        b.board(&["add", "backlog", "x"]).failed(4),
        not_ready(&b, "it has no remote origin")
    );

    // History is the counter; a shallow clone has only its tip.
    a.board(&["add", "backlog", "x"]).ok();
    a.board(&["drop", "b1"]).ok();
    fs::remove_dir_all(b.data()).unwrap();
    let url = format!("file://{}", world.remote().display());
    b.git(
        &b.home().join("Work"),
        ["clone", "-q", "--depth", "1", &url, "board-data"],
    );
    assert_eq!(
        b.board(&["add", "backlog", "x"]).failed(4),
        not_ready(
            &b,
            "it is a shallow clone, and the next id is counted from the whole history"
        )
    );

    fs::remove_dir_all(b.data()).unwrap();
    fs::create_dir(b.data()).unwrap();
    assert_eq!(
        b.board(&["add", "backlog", "x"]).failed(4),
        not_ready(&b, "it has no .git directory")
    );
}
