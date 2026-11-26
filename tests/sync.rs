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

/// The remote's items of a kind's directory (`backlog`), each as its id and
/// title, by id.
fn listed(world: &World, dir: &str) -> Vec<String> {
    let mut items: Vec<(u32, String)> = world
        .remote_files(&format!("proj/{dir}"))
        .into_iter()
        .map(|path| {
            let name = path.rsplit('/').next().unwrap().trim_end_matches(".md");
            let title = text(world.remote_file(&path))
                .lines()
                .find_map(|line| line.strip_prefix("# ").map(str::to_owned))
                .unwrap_or_default();
            (name[1..].parse().unwrap(), format!("{name} {title}"))
        })
        .collect();
    items.sort();
    items.into_iter().map(|(_, item)| item).collect()
}

/// The clone is where the remote is, with nothing changed by hand.
#[track_caller]
fn in_step(world: &World, machine: &Machine) {
    assert_eq!(machine.main(), world.remote_main());
    assert_eq!(machine.status(), "");
}

// ---- the commands ----

/// A file's text with its `created:` stamp, which is the clock's, as `T`.
fn stamped(bytes: Option<Vec<u8>>) -> String {
    text(bytes)
        .lines()
        .map(|line| match line.strip_prefix("created: ") {
            Some(stamp) => {
                assert!(stamp.ends_with('Z') && stamp.len() == 20, "{stamp}");
                "created: T\n".to_owned()
            }
            None => format!("{line}\n"),
        })
        .collect()
}

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
    // No check: a chore whose title says it all.
    assert_eq!(
        a.board(&["add", "chore", "Water the plants", "--group", "Home"])
            .ok(),
        "c2\n"
    );
    // Without `--group`: `Default`, for both kinds; with it, that group.
    assert_eq!(a.board(&["add", "chore", "Sweep"]).ok(), "c3\n");
    assert_eq!(
        a.board(&["add", "backlog", "Export", "--group", "Later"])
            .ok(),
        "b3\n"
    );
    assert_eq!(
        stamped(world.remote_file("proj/chores/c3.md")),
        "id: c3\ngroup: Default\ncreated: T\n\n# Sweep\n"
    );
    assert_eq!(
        stamped(world.remote_file("proj/backlog/b3.md")),
        "id: b3\ngroup: Later\ncreated: T\n\n# Export\n"
    );
    assert_eq!(
        stamped(world.remote_file("proj/backlog/b1.md")),
        "id: b1\ngroup: Default\ncreated: T\n\n# Dark mode\n"
    );
    assert_eq!(
        stamped(world.remote_file("proj/backlog/b2.md")),
        "id: b2\ngroup: Default\nfrom: 126\ncreated: T\n\n# Faster search\n"
    );
    assert_eq!(
        stamped(world.remote_file("proj/chores/c1.md")),
        "id: c1\ngroup: Home\ncheck: login works\nfrom: b2\ncreated: T\n\n\
         # Rotate the key\n"
    );
    assert_eq!(
        stamped(world.remote_file("proj/chores/c2.md")),
        "id: c2\ngroup: Home\ncreated: T\n\n# Water the plants\n"
    );
    assert_eq!(
        world.remote_log(),
        [
            "proj: add b3",
            "proj: add c3",
            "proj: add c2",
            "proj: add c1",
            "proj: add b2",
            "proj: add b1",
            "start"
        ]
    );
    // The clone's files are the remote's.
    in_step(&world, &a);
    assert_eq!(
        a.board(&["ls", "backlog"]).ok(),
        "## Default\nb1 Dark mode\nb2 Faster search  (from 126)\n## Later\nb3 Export\n"
    );
    assert_eq!(
        a.board(&["ls", "chores"]).ok(),
        "## Default\nc3 Sweep\n## Home\nc1 Rotate the key. Check: login works  (b2)\n\
         c2 Water the plants\n"
    );
    assert_eq!(a.board(&["lint"]).ok(), "ok\n");
}

#[test]
fn done_and_drop_delete_the_files_all_or_nothing() {
    let world = World::new();
    let a = world.a();
    for chore in ["one", "two", "three"] {
        a.board(&["add", "chore", chore, "--group", "Home"]).ok();
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
    assert_eq!(world.remote_files("proj/chores"), ["proj/chores/c2.md"]);
    assert_eq!(world.remote_files("proj/backlog"), Vec::<String>::new());
    assert_eq!(
        world.remote_log()[..2],
        ["proj: dropped b1", "proj: done c3 c1"]
    );
    in_step(&world, &a);
}

/// `retitle` changes a title or a chore's check in one commit and keeps
/// its id, group, reference, body and every other key; `--check ""`
/// removes the check.
#[test]
fn retitle_changes_an_item_s_title_or_check_and_keeps_the_rest() {
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
    a.board(&["add", "backlog", "Dark mode", "--from", "c1"])
        .ok();
    // A key the tool does not know, typed by hand: kept.
    let c1 = text(a.file("proj/chores/c1.md")).replace("created:", "owner: me\ncreated:");
    a.write("proj/chores/c1.md", &c1);
    assert_eq!(
        a.board(&["retitle", "c1", "Rotate both keys"]).ok(),
        "retitled c1\n"
    );
    assert_eq!(
        a.board(&["ls", "chores"]).ok(),
        "## Home\nc1 Rotate both keys. Check: login works  (126)\n"
    );
    a.board(&["retitle", "c1", "--check", "the old key fails"])
        .ok();
    a.board(&["retitle", "b1", "Dark theme"]).ok();
    assert_eq!(
        stamped(world.remote_file("proj/chores/c1.md")),
        "id: c1\ngroup: Home\ncheck: the old key fails\nfrom: 126\nowner: me\n\
         created: T\n\n# Rotate both keys\n\nthe steps\n"
    );
    assert_eq!(
        stamped(world.remote_file("proj/backlog/b1.md")),
        "id: b1\ngroup: Default\nfrom: c1\ncreated: T\n\n# Dark theme\n"
    );
    a.board(&["retitle", "c1", "--check", ""]).ok();
    assert_eq!(
        a.board(&["ls", "chores"]).ok(),
        "## Home\nc1 Rotate both keys  (126)\n"
    );
    assert_eq!(
        a.board(&["show", "c1"]).ok(),
        text(a.file("proj/chores/c1.md"))
    );
    assert_eq!(
        world.remote_log()[..4],
        [
            "proj: retitle c1",
            "proj: retitle b1",
            "proj: retitle c1",
            "proj: retitle c1"
        ]
    );
    assert_eq!(a.board(&["lint"]).ok(), "ok\n");
    in_step(&world, &a);
}

#[test]
fn retitle_refuses_what_names_no_item_or_no_change() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "chore", "Rotate the key", "--group", "Home"])
        .ok();
    a.board(&["add", "backlog", "Dark mode"]).ok();
    for args in [
        &["retitle", "c1"][..],
        &["retitle", "128", "x"],
        &["retitle", "b1", "--check", "y"],
        &["retitle", "c1", "--check", "two\nlines"],
    ] {
        a.board(args).failed(2);
    }
    assert!(
        a.board(&["retitle", "128", "x"])
            .failed(2)
            .contains("a brief's title is in its file")
    );
    assert_eq!(
        a.board(&["retitle", "c9", "x"]).failed(1),
        "board: retitle c9: no such chore\n"
    );
    assert_eq!(world.remote_log()[0], "proj: add b1");
}

/// A body is Markdown, written through `board`: given at `add --body`,
/// appended by `amend` after a blank line, replaced by `amend --replace`
/// (empty, it is gone); `show` gives it back as stored, and `done` deletes
/// it with its file.
#[test]
fn add_body_amend_and_replace_write_an_item_s_body() {
    let world = World::new();
    let a = world.a();
    let body =
        "Run the query:\n\n```sql\nselect count(*)\n  from orders;\n```\n\n- a list\n  - nested\n";
    assert_eq!(
        a.board_with_input(
            &[
                "add",
                "chore",
                "Check the orders",
                "--group",
                "Deploy",
                "--check",
                "the query below returns 0",
                "--body"
            ],
            body.as_bytes(),
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
        a.board_with_input(&["amend", "c1"], b"## Then\n- c9 not an item")
            .ok(),
        "amended c1\n"
    );
    assert_eq!(
        a.board(&["show", "c1"]).ok(),
        text(a.file("proj/chores/c1.md"))
    );
    assert_eq!(
        stamped(world.remote_file("proj/chores/c1.md")),
        format!(
            "id: c1\ngroup: Deploy\ncheck: the query below returns 0\ncreated: T\n\n\
             # Check the orders\n\n{body}\n## Then\n- c9 not an item\n"
        )
    );
    assert_eq!(
        a.board(&["ls", "chores"]).ok(),
        "## Deploy\nc1 Check the orders. Check: the query below returns 0\n"
    );
    // Replace: in place of the body, or of none; empty removes it.
    assert_eq!(
        a.board_with_input(&["amend", "b1", "--replace"], b"why: battery")
            .ok(),
        "amended b1\n"
    );
    assert!(text(a.file("proj/backlog/b1.md")).ends_with("# Dark mode\n\nwhy: battery\n"));
    a.board_with_input(&["amend", "b1", "--replace"], b"").ok();
    assert_eq!(
        stamped(world.remote_file("proj/backlog/b1.md")),
        "id: b1\ngroup: Default\ncreated: T\n\n# Dark mode\n"
    );
    a.board_with_input(&["amend", "b1", "--replace"], b"back")
        .ok();
    assert!(text(a.file("proj/backlog/b1.md")).ends_with("# Dark mode\n\nback\n"));
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
    assert_eq!(world.remote_files("proj/chores"), Vec::<String>::new());
    in_step(&world, &a);
}

#[test]
fn a_body_command_that_says_nothing_or_names_no_item_is_refused() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "backlog", "Dark mode"]).ok();
    let before = world.remote_file("proj/backlog/b1.md");
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
    assert_eq!(world.remote_file("proj/backlog/b1.md"), before);
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

/// `group` writes the `group:` line of each item it names, chores and
/// backlog items mixed, in one commit, and nothing else of the files.
#[test]
fn group_writes_the_group_lines_in_one_commit() {
    let world = World::new();
    let a = world.a();
    for item in ["one", "two"] {
        a.board(&["add", "backlog", item]).ok();
    }
    for (chore, group) in [("x", "Home"), ("y", "Home"), ("z", "Work")] {
        a.board(&["add", "chore", chore, "--group", group]).ok();
    }
    let files = [
        "proj/backlog/b2.md",
        "proj/chores/c1.md",
        "proj/chores/c3.md",
    ];
    let before = files.map(|file| text(world.remote_file(file)));
    let commits = world.remote_log().len();
    assert_eq!(
        a.board(&["group", "c1", "b2", "c3", "b2", "--to", "Later"])
            .ok(),
        "grouped c1 b2 c3 under Later\n"
    );
    assert_eq!(world.remote_log().len(), commits + 1);
    assert_eq!(world.remote_log()[0], "proj: grouped c1 b2 c3 under Later");
    let mut changed = world.remote_changed("main");
    changed.sort();
    assert_eq!(changed, files);
    for (file, before) in files.iter().zip(before) {
        let group = before
            .lines()
            .find(|line| line.starts_with("group: "))
            .unwrap()
            .to_owned();
        assert_eq!(
            text(world.remote_file(file)),
            before.replace(&group, "group: Later"),
            "{file}"
        );
    }
    assert_eq!(
        a.board(&["ls", "chores"]).ok(),
        "## Home\nc2 y\n## Later\nc1 x\nc3 z\n"
    );
    assert_eq!(
        a.board(&["ls", "backlog"]).ok(),
        "## Default\nb1 one\n## Later\nb2 two\n"
    );
    in_step(&world, &a);

    // An item there already is left out of the commit and of the print.
    assert_eq!(
        a.board(&["group", "c1", "c2", "--to", "Later"]).ok(),
        "grouped c2 under Later\n"
    );
    assert_eq!(world.remote_changed("main"), ["proj/chores/c2.md"]);
    // All of them: said so, and no commit.
    let log = world.remote_log();
    assert_eq!(
        a.board(&["group", "c1", "b2", "--to", "Later"]).ok(),
        "nothing to group: all are under Later already\n"
    );
    assert_eq!(world.remote_log(), log);
    in_step(&world, &a);
}

#[test]
fn group_refuses_an_unknown_id_and_a_missing_name_and_changes_nothing() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "backlog", "one"]).ok();
    a.board(&["add", "chore", "x"]).ok();
    let log = world.remote_log();
    let files = world.remote_blobs("proj");
    assert_eq!(
        a.board(&["group", "c1", "b9", "b1", "--to", "Later"])
            .failed(1),
        "board: group b9: no such backlog item\n"
    );
    // Without `--to` the last id is no name: refused before the board is
    // read.
    for args in [
        &["group", "c1", "b1"][..],
        &["group", "--to", "Later"],
        &["group", "c1", "--to", " "],
        &["group", "128", "--to", "Later"],
    ] {
        let said = a.board(args).failed(2);
        assert!(said.starts_with("board: "), "{args:?}: {said}");
    }
    assert_eq!(world.remote_log(), log);
    assert_eq!(world.remote_blobs("proj"), files);
    in_step(&world, &a);
}

/// An item file without a `group:` line is listed and no command refuses
/// it; `group` puts the line right after `id:`.
#[test]
fn an_item_without_a_group_takes_every_item_command() {
    let world = World::new();
    let a = world.a();
    a.write(
        "proj/chores/c1.md",
        "id: c1\nfrom: 126\ncreated: x\n\n# Water the plants\n",
    );
    a.write("proj/chores/c2.md", "id: c2\ncreated: x\n\n# Sweep\n");
    a.write(
        "proj/backlog/b1.md",
        "id: b1\nowner: me\ncreated: x\n\n# Dark mode\n",
    );
    a.write("proj/backlog/b2.md", "id: b2\ncreated: x\n\n# Search\n");
    // Every write says what lint finds as a note: the missing groups, of
    // which some are left at every call below.
    let ok = |args: &[&str], input: &[u8]| {
        let ran = a.board_with_input(args, input);
        assert_eq!(ran.code, 0, "{args:?}: {ran:?}");
        assert!(!ran.stderr.is_empty(), "no lint note: {ran:?}");
        assert!(
            ran.stderr
                .lines()
                .all(|line| line.ends_with("gives it `group: Default`")),
            "{ran:?}"
        );
        String::from_utf8(ran.stdout).unwrap()
    };
    ok(&["sync"], b"");
    assert_eq!(
        a.board(&["ls", "chores"]).ok(),
        "## \nc1 Water the plants  (126)\nc2 Sweep\n"
    );
    ok(&["retitle", "c1", "Water them"], b"");
    ok(&["retitle", "b1", "Dark theme"], b"");
    ok(&["amend", "b1"], b"why");
    ok(&["amend", "c1"], b"how");
    assert_eq!(
        ok(&["group", "c1", "b1", "--to", "Home"], b""),
        "grouped c1 b1 under Home\n"
    );
    assert_eq!(
        text(world.remote_file("proj/chores/c1.md")),
        "id: c1\ngroup: Home\nfrom: 126\ncreated: x\n\n# Water them\n\nhow\n"
    );
    assert_eq!(
        text(world.remote_file("proj/backlog/b1.md")),
        "id: b1\ngroup: Home\nowner: me\ncreated: x\n\n# Dark theme\n\nwhy\n"
    );
    assert_eq!(ok(&["done", "c2"], b""), "done c2\n");
    assert_eq!(a.board(&["drop", "b2"]).ok(), "dropped b2\n");
    in_step(&world, &a);
}

/// `move` went with the order of chores and backlog items.
#[test]
fn move_is_no_command() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "backlog", "one"]).ok();
    a.board(&["add", "backlog", "two"]).ok();
    let said = a.board(&["move", "b2", "--before", "b1"]).failed(2);
    assert!(
        said.starts_with("board: unrecognized subcommand 'move'"),
        "{said}"
    );
}

/// A regrouping allocates no id: offline it is committed on the clone, and
/// the next write that reaches the remote pushes it.
#[test]
fn offline_a_group_waits_on_the_clone() {
    let world = World::new();
    let a = world.a();
    for item in ["one", "two"] {
        a.board(&["add", "backlog", item]).ok();
    }
    a.set_online(false);
    let ran = a.board(&["group", "b2", "--to", "Later"]);
    assert_eq!(
        (ran.code, ran.stdout.as_slice()),
        (0, &b"grouped b2 under Later\n"[..])
    );
    assert!(
        ran.stderr
            .starts_with("board: not pushed: 1 commit (fatal: "),
        "{ran:?}"
    );
    assert_eq!(
        a.board(&["ls", "backlog"]).ok(),
        "## Default\nb1 one\n## Later\nb2 two\n"
    );
    assert_eq!(world.remote_log()[0], "proj: add b2");
    a.set_online(true);
    a.board(&["sync"]).ok();
    assert_eq!(world.remote_log()[0], "proj: grouped b2 under Later");
    in_step(&world, &a);
}

#[test]
fn what_is_no_command_line_for_a_write_is_a_usage_error() {
    let world = World::new();
    let a = world.a();
    for args in [
        &["done", "b41"][..],
        &["drop", "c12"],
        &["done"],
        &["add", "chore", "text", "--group", " "],
        &["add", "backlog", "text", "--group", ""],
        // A chore without a check leaves the flag out.
        &["add", "chore", "text", "--group", "Home", "--check", ""],
        &[
            "add",
            "chore",
            "text",
            "--group",
            "Home",
            "--check",
            "two\nlines",
        ],
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
fn lint_findings_after_a_write_are_notes() {
    let world = World::new();
    let a = world.a();
    a.write("proj/chores/c1.md", "id: c1\ncreated: x\n\n# no group\n");
    a.commit_and_push("by hand");
    let ran = a.board(&["add", "backlog", "x"]);
    assert_eq!(
        (ran.code, ran.stdout.as_slice(), ran.stderr.as_str()),
        (
            0,
            &b"b1\n"[..],
            "board: lint: proj/chores/c1.md: no `group:` line, or an empty one; `board migrate` gives it `group: Default`\n"
        )
    );
}

#[test]
fn sync_brings_the_clone_to_the_remote() {
    let world = World::new();
    let (a, b) = (world.a(), world.b());
    a.board(&["add", "backlog", "from a"]).ok();
    assert_eq!(b.file("proj/backlog/b1.md"), None);
    assert_eq!(b.board(&["sync"]).ok(), "synced\n");
    assert_eq!(b.file("proj/backlog/b1.md"), a.file("proj/backlog/b1.md"));
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
    assert_eq!(on_remote(&world, "proj/backlog/b1.md", "# x\n"), 1);
}

// ---- the next id ----

/// The ids of the item files ever added count, as do the ids the old list
/// files (`chores.md`, `backlog.md`) ever held: a board converted from
/// them has the ids of what was done before only there. The list-file
/// cases below are of that history.
#[test]
fn an_id_is_never_given_twice_even_after_its_file_is_gone() {
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
fn a_file_made_by_hand_raises_the_next_id() {
    let world = World::new();
    let a = world.a();
    a.write(
        "proj/backlog/b40.md",
        "id: b40\ngroup: Default\ncreated: x\n\n# by hand\n",
    );
    a.commit_and_push("by hand");
    assert_eq!(a.board(&["add", "backlog", "x"]).ok(), "b41\n");
}

/// An id only the old list file's history holds stays used.
#[test]
fn an_id_that_only_a_list_file_ever_held_stays_used() {
    let world = World::new();
    let b = world.b();
    b.write("proj/chores.md", "## g\n- c7 done long ago. Check: x\n");
    b.commit_and_push("the list");
    b.git(&b.data(), ["rm", "-q", "proj/chores.md"]);
    b.commit_and_push("converted");
    assert_eq!(
        world.a().board(&["add", "chore", "x", "--group", "g"]).ok(),
        "c8\n"
    );
}

/// The old list file taken out, as its conversion to item files does: the
/// ids it held are then in its history only.
fn converted(machine: &Machine, file: &str) {
    machine.git(&machine.data(), ["rm", "-q", file]);
    machine.commit_and_push("converted");
}

/// An item file added and deleted on a merged side branch: its id stays
/// used too.
#[test]
fn an_item_file_added_and_deleted_on_a_merged_side_branch_is_used() {
    let world = World::new();
    let b = world.b();
    let data = b.data();
    let item = |id: u32| format!("id: b{id}\ngroup: Default\ncreated: x\n\n# x\n");
    b.write("proj/backlog/b1.md", item(1));
    b.commit_and_push("one");
    b.git(&data, ["switch", "-q", "-c", "side"]);
    b.write("proj/backlog/b7.md", item(7));
    b.git(&data, ["add", "proj"]);
    b.git(&data, ["commit", "-q", "-m", "side add"]);
    b.git(&data, ["rm", "-q", "proj/backlog/b7.md"]);
    b.git(&data, ["commit", "-q", "-m", "side delete"]);
    b.git(&data, ["switch", "-q", "main"]);
    b.git(
        &data,
        ["merge", "-q", "--no-ff", "-m", "merge side", "side"],
    );
    b.git(&data, ["push", "-q", "origin", "main"]);
    assert_eq!(world.a().board(&["add", "backlog", "x"]).ok(), "b8\n");
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
    converted(&b, "proj/backlog.md");
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
    converted(&b, "proj/chores.md");
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
    converted(&b, "proj/backlog.md");
    assert_eq!(world.a().board(&["add", "backlog", "x"]).ok(), "b10\n");
}

#[test]
fn the_last_id_ends_allocation_for_its_list() {
    let world = World::new();
    let a = world.a();
    a.write("proj/backlog.md", "- b4294967295 the last\n");
    a.commit_and_push("by hand");
    converted(&a, "proj/backlog.md");
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
        (0, &b"b1\n"[..], "board: retry 2/8\n")
    );
    assert_eq!(listed(&world, "backlog"), ["b1 x"]);
    in_step(&world, &a);
}

/// Eight refused pushes count as offline: an id is refused, since only a
/// push may publish it, and any other write is committed on the clone and
/// pushed by the next write that gets through.
#[test]
fn eight_refused_pushes_refuse_an_id_and_keep_other_writes_on_the_clone() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "chore", "x", "--group", "g", "--check", "x"])
        .ok();
    let before = world.remote_main();
    world.reject_pushes(17);
    let retries: String = (2..=8).map(|n| format!("board: retry {n}/8\n")).collect();
    let refused = " -> main (pre-receive hook declined))\n";

    let said = a.board(&["add", "backlog", "x"]).failed(3);
    assert!(
        said.starts_with(&format!(
            "{retries}board: add backlog: not pushed after 8 attempts (! [remote rejected] "
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
    assert_eq!(a.file("proj/chores/c1.md"), None);

    world.reject_pushes(0);
    assert_eq!(a.board(&["add", "backlog", "y"]).ok(), "b1\n");
    assert_eq!(listed(&world, "chores"), Vec::<String>::new());
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
        (0, &b"b2\n"[..], "board: retry 2/8\n")
    );
    assert_eq!(listed(&world, "backlog"), ["b1 from b", "b2 from a"]);
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
        (0, &b"b1\n"[..], "board: retry 2/8\n")
    );
    assert_eq!(listed(&world, "backlog"), ["b1 once"]);

    // Run again, `done` would find c1 gone and refuse.
    a.hook_once("pre-push", pre_push::LOST_ACK);
    let ran = a.board(&["done", "c1"]);
    assert_eq!(
        (ran.code, ran.stdout.as_slice(), ran.stderr.as_str()),
        (0, &b"done c1\n"[..], "board: retry 2/8\n")
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
        (0, &b"b1\n"[..], "board: retry 2/8\nboard: retry 3/8\n")
    );
    assert_eq!(listed(&world, "backlog"), ["b1 once"]);
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
    assert_eq!(a.file("proj/backlog/b1.md"), None);
    // The lock died with it.
    assert_eq!(a.board(&["add", "backlog", "next"]).ok(), "b1\n");
    assert_eq!(listed(&world, "backlog"), ["b1 next"]);
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
    assert_eq!(listed(&world, "backlog"), ["b1 landed"]);
    assert_eq!(a.file("proj/backlog/b1.md"), None);
    assert_eq!(a.board(&["add", "backlog", "next"]).ok(), "b2\n");
    assert_eq!(
        a.board(&["ls", "backlog"]).ok(),
        "## Default\nb1 landed\nb2 next\n"
    );
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
    a.write(
        "proj/backlog/b5.md",
        "id: b5\ngroup: Default\ncreated: x\n\n# by hand\n",
    );
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

    a.set_online(true);
    let ran = a.board(&["add", "backlog", "next"]);
    assert_eq!(
        (ran.code, ran.stdout.as_slice(), ran.stderr.as_str()),
        (0, &b"b6\n"[..], "")
    );
    assert_eq!(listed(&world, "chores"), Vec::<String>::new());
    assert_eq!(listed(&world, "backlog"), ["b5 by hand", "b6 next"]);
    assert_eq!(
        world.remote_log()[..3],
        ["proj: add b6", "board: hand edits", "proj: done c1"]
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
    assert_eq!(listed(&world, "backlog"), ["b1 first", "b2 second"]);
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
    a.write(
        "proj/chores/c1.md",
        "id: c1\ngroup: g\ncreated: x\n\n# by hand\n",
    );
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
        "proj/backlog/* -diff\nproj/chores/* diff=garbage\n\
         proj/backlog.md -diff\nproj/chores.md diff=garbage\n",
    )
    .unwrap();

    a.board(&["add", "backlog", "one"]).ok();
    a.board(&["add", "backlog", "two"]).ok();
    a.write(
        "proj/backlog/b5.md",
        "id: b5\ngroup: Default\ncreated: x\n\n# signed\n",
    );
    a.git(&a.data(), ["add", "proj"]);
    a.git(
        &a.data(),
        [
            "-c",
            &format!("gpg.program={signer}"),
            "commit",
            "-q",
            "-S",
            "-m",
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
    assert_eq!(listed(&world, "backlog"), ["b6 three"]);
    assert_eq!(a.board(&["ls", "backlog"]).ok(), "## Default\nb6 three\n");
    in_step(&world, &a);
    // An untracked board file is seen with untracked files hidden; the
    // commit of hand edits and the merge go by the tool's settings, not
    // these; hooks that refuse every commit do not run.
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
    let hand = "id: c1\ngroup: g\ncreated: x\n\n# hand\n";
    a.write("shop/chores/c1.md", hand);
    let b6 = text(a.file("proj/backlog/b6.md")).replace("# three", "# three, edited");
    a.write("proj/backlog/b6.md", &b6);
    let ran = a.board(&["add", "backlog", "four"]);
    assert_eq!(
        (ran.code, ran.stdout.as_slice(), ran.stderr.as_str()),
        (0, &b"b8\n"[..], "board: merged 1 local commit\n")
    );
    assert_eq!(
        listed(&world, "backlog"),
        ["b6 three, edited", "b7 from b", "b8 four"]
    );
    assert_eq!(text(world.remote_file("shop/chores/c1.md")), hand);
    in_step(&world, &a);
}

// ---- hand edits and merges ----

/// An item file as a hand edit writes it.
fn by_hand(id: &str, title: &str) -> String {
    format!("id: {id}\ngroup: g\ncreated: x\n\n# {title}\n")
}

/// Every write commits the board files changed by hand first, in every
/// project: an edited item, a well-named file added by hand. The tool gives
/// no id: a file's name is its id, and a list file of the old kind is
/// committed too, for `migrate` to convert. What is not the board's (a
/// misnamed file, a copy) stays untracked.
#[test]
fn hand_edits_are_committed_and_a_misnamed_file_is_not() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "chore", "one", "--group", "Home"]).ok();
    let edited = text(a.file("proj/chores/c1.md")).replace("# one", "# one, edited");
    a.write("proj/chores/c1.md", &edited);
    a.write("shop/backlog/b7.md", by_hand("b7", "typed"));
    a.write("proj/backlog/notes.md", "not an item\n");
    a.write("shop/chores.md", "- c9 the old kind\n");
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
        "board: lint: proj/backlog/notes.md: not named as the board names its files \
         (briefs/[done/]<number>-<slug>.md, chores/c<id>.md, backlog/b<id>.md), so it is \
         not synced\n\
         board: lint: proj/briefs/128-x.md.local: unmerged local version of brief 128; \
         merge it by hand, then delete this file\n"
    );
    assert_eq!(text(world.remote_file("proj/chores/c1.md")), edited);
    assert_eq!(
        text(world.remote_file("shop/backlog/b7.md")),
        by_hand("b7", "typed")
    );
    assert_eq!(
        text(world.remote_file("shop/chores.md")),
        "- c9 the old kind\n"
    );
    assert_eq!(
        world.remote_log()[..2],
        ["proj: add b1", "board: hand edits"]
    );
    for untracked in [
        "README.md",
        "proj/backlog/notes.md",
        "proj/briefs/128-x.md.local",
    ] {
        assert_eq!(world.remote_file(untracked), None, "{untracked}");
    }
    assert_eq!(a.main(), world.remote_main());
    assert_eq!(
        a.status(),
        "?? README.md\n?? proj/backlog/notes.md\n?? proj/briefs/128-x.md.local\n"
    );
}

/// Both clones changed items: each item is its own file, so edits of
/// different items are both kept, adds of both sides stay, and the clone's
/// commits join the remote's by a merge commit.
#[test]
fn diverged_clones_keep_each_side_s_edits_of_different_items() {
    let world = World::new();
    let (a, b) = (world.a(), world.b());
    for item in ["one", "two"] {
        a.board(&["add", "backlog", item]).ok();
    }
    b.board(&["sync"]).ok();
    let edit = |machine: &Machine, id: &str, title: &str| {
        let file = format!("proj/backlog/{id}.md");
        let old = text(machine.file(&file));
        let old_title = old.lines().find(|line| line.starts_with("# ")).unwrap();
        machine.write(&file, old.replace(old_title, &format!("# {title}")));
    };
    edit(&b, "b1", "one, remote");
    assert_eq!(b.board(&["add", "backlog", "three"]).ok(), "b3\n");
    edit(&a, "b2", "two, local");
    let ran = a.board(&["add", "backlog", "four"]);
    assert_eq!(
        (ran.code, ran.stdout.as_slice(), ran.stderr.as_str()),
        (0, &b"b4\n"[..], "board: merged 1 local commit\n")
    );
    assert_eq!(
        listed(&world, "backlog"),
        ["b1 one, remote", "b2 two, local", "b3 three", "b4 four"]
    );
    let log = world.remote_log();
    assert_eq!(log[..2], ["proj: add b4", "board: merge"]);
    assert!(log.contains(&"board: hand edits".to_owned()), "{log:?}");
    in_step(&world, &a);
    b.board(&["sync"]).ok();
    in_step(&world, &b);
    assert_eq!(a.board(&["lint"]).ok(), "ok\n");
}

/// One item both clones changed: the remote's version stays, the clone's
/// is saved beside it as `.local`, the note says so, and lint reports the
/// copy until it is deleted.
#[test]
fn an_item_changed_on_both_clones_keeps_the_remote_s_and_saves_the_local_one() {
    let world = World::new();
    let (a, b) = (world.a(), world.b());
    a.board(&["add", "chore", "Rotate the key", "--group", "Home"])
        .ok();
    b.board(&["sync"]).ok();
    let file = "proj/chores/c1.md";
    let base = text(a.file(file));
    b.write(file, format!("{base}\nfrom b\n"));
    b.board(&["sync"]).ok();
    a.write(file, format!("{base}\nfrom a\n"));
    let copy = a.data().join("proj/chores/c1.md.local");
    let finding = "board: lint: proj/chores/c1.md.local: unmerged local version of c1; \
                   merge it by hand, then delete this file\n";
    let ran = a.board(&["sync"]);
    assert_eq!(
        (ran.code, ran.stderr),
        (
            0,
            format!(
                "board: merged 1 local commit\n\
                 board: conflict: c1 changed here and on the remote; the remote version \
                 is kept, yours is saved as {}\n{finding}",
                copy.display()
            )
        )
    );
    for kept in [world.remote_file(file), a.file(file)] {
        assert_eq!(text(kept), format!("{base}\nfrom b\n"));
    }
    assert_eq!(
        text(a.file("proj/chores/c1.md.local")),
        format!("{base}\nfrom a\n")
    );
    assert_eq!(a.board(&["lint"]).out(1), &finding["board: lint: ".len()..]);
    fs::remove_file(&copy).unwrap();
    assert_eq!(a.board(&["lint"]).ok(), "ok\n");
    in_step(&world, &a);
}

/// Deleted on one clone and edited on the other, either way round: the
/// delete wins everywhere, and no `.local` copy is made (owner,
/// 2026-11-25: a done chore that keeps nagging is worse than an edit that
/// is in the history).
#[test]
fn a_delete_wins_over_an_edit_either_way_round_without_a_copy() {
    for remote_deletes in [true, false] {
        let world = World::new();
        let (a, b) = (world.a(), world.b());
        a.board_with_input(&["add", "backlog", "Dark mode", "--body"], b"old")
            .ok();
        a.board(&["add", "backlog", "Later"]).ok();
        b.board(&["sync"]).ok();
        let (deleter, editor) = if remote_deletes { (&a, &b) } else { (&b, &a) };
        // Offline, so the edit waits on its clone while the other drops.
        editor.set_online(false);
        let ran = editor.board_with_input(&["amend", "b1", "--replace"], b"new");
        assert_eq!((ran.code, ran.stdout.as_slice()), (0, &b"amended b1\n"[..]));
        if remote_deletes {
            assert_eq!(deleter.board(&["drop", "b1"]).ok(), "dropped b1\n");
            editor.set_online(true);
        } else {
            editor.set_online(true);
            editor.board(&["sync"]).ok();
            // The deleter drops offline and syncs after the edit landed.
            deleter.set_online(false);
            deleter.board(&["drop", "b1"]);
            deleter.set_online(true);
        }
        for machine in [editor, deleter, editor] {
            let ran = machine.board(&["sync"]);
            assert_eq!(ran.code, 0, "{ran:?}");
            assert!(!ran.stderr.contains("conflict"), "{ran:?}");
        }
        assert_eq!(listed(&world, "backlog"), ["b2 Later"], "{remote_deletes}");
        for machine in [&a, &b] {
            assert_eq!(machine.file("proj/backlog/b1.md"), None);
            assert_eq!(machine.file("proj/backlog/b1.md.local"), None);
            in_step(&world, machine);
        }
    }
}

/// Two clones adding offline-free at once: both kept, with different ids.
#[test]
fn adds_on_two_clones_are_both_kept_with_different_ids() {
    let world = World::new();
    let (a, b) = (world.a(), world.b());
    assert_eq!(a.board(&["add", "backlog", "from a"]).ok(), "b1\n");
    assert_eq!(b.board(&["add", "backlog", "from b"]).ok(), "b2\n");
    a.board(&["sync"]).ok();
    assert_eq!(listed(&world, "backlog"), ["b1 from a", "b2 from b"]);
    in_step(&world, &a);
    in_step(&world, &b);
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

/// A project directory renamed by hand and synced: the ids of the files
/// it moved stay used, as files added under the new name. One deleted
/// before the rename frees its id, as for a brief: the history of the old
/// directory is not followed.
#[test]
fn a_project_directory_renamed_by_hand_keeps_the_ids_of_its_files() {
    let world = World::new();
    let b = world.b();
    b.write("old/chores/c1.md", by_hand("c1", "one"));
    b.write("old/chores/c5.md", by_hand("c5", "five"));
    b.board(&["--board", "old", "sync"]).ok();
    fs::rename(b.data().join("old"), b.data().join("proj")).unwrap();
    assert_eq!(b.board(&["sync"]).ok(), "synced\n");
    assert_eq!(
        world.a().board(&["add", "chore", "x", "--group", "g"]).ok(),
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
        &format!(
            "mkdir -p proj/chores && printf '%s' {} >proj/chores/c1.md\n",
            quote(&by_hand("c1", "by hand"))
        ),
    );
    assert_eq!(a.board(&["add", "backlog", "two"]).ok(), "b2\n");
    assert_eq!(a.main(), world.remote_main());
    assert_eq!(a.status(), "?? proj/chores/c1.md\n");

    // The file the write changes: committed and merged. The remote's
    // version stands (the write's), the save is kept beside it.
    a.board(&["group", "b1", "--to", "Soon"]).ok();
    let saved = text(a.file("proj/backlog/b1.md")).replace("# one", "# one, edited");
    a.hook_once(
        "pre-push",
        &format!("printf '%s' {} >proj/backlog/b1.md\n", quote(&saved)),
    );
    let ran = a.board(&["group", "b1", "--to", "Later"]);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert!(
        ran.stderr.starts_with("board: merged 1 local commit\n"),
        "{ran:?}"
    );
    assert_eq!(
        a.board(&["ls", "backlog"]).ok(),
        "## Default\nb2 two\n## Later\nb1 one\n"
    );
    assert_eq!(text(a.file("proj/backlog/b1.md.local")), saved);
    fs::remove_file(a.data().join("proj/backlog/b1.md.local")).unwrap();
    assert_eq!(a.status(), "");

    assert_eq!(a.board(&["add", "backlog", "three"]).ok(), "b3\n");
    assert_eq!(listed(&world, "chores"), ["c1 by hand"]);
    in_step(&world, &a);
}

/// A file deleted while a write that changes it pushes: git's fast-forward
/// would write the file back without refusing, so the delete is absorbed
/// and merged first, and wins over the write as any delete over an edit.
#[test]
fn a_file_deleted_during_the_push_of_its_write_stays_deleted() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "backlog", "one"]).ok();
    a.board(&["add", "backlog", "two"]).ok();
    a.hook_once("pre-push", "rm proj/backlog/b1.md\n");
    let ran = a.board(&["retitle", "b1", "one, renamed"]);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert!(!a.data().join("proj/backlog/b1.md").exists(), "{ran:?}");
    assert_eq!(a.board(&["ls", "backlog"]).ok(), "## Default\nb2 two\n");
    assert_eq!(a.status(), "");

    // The next write pushes the delete.
    a.board(&["add", "backlog", "three"]).ok();
    assert_eq!(listed(&world, "backlog"), ["b2 two", "b3 three"]);
    in_step(&world, &a);
}

/// Killed after its push reached the remote, with the clone's own commits
/// merged into that push: the clone is behind, and the next write catches up
/// without merging the hand edit again.
#[test]
fn a_command_killed_after_pushing_a_merge_is_healed_without_a_second_merge() {
    let world = World::new();
    let (a, b) = (world.a(), world.b());
    b.board(&["add", "backlog", "from b"]).ok();
    a.write("proj/backlog/b5.md", by_hand("b5", "by hand"));
    a.hook_once("pre-push", pre_push::KILL_AFTER_PUSH);
    a.board_killed(&["add", "backlog", "landed"]);
    assert_eq!(
        listed(&world, "backlog"),
        ["b1 from b", "b5 by hand", "b6 landed"]
    );
    assert_eq!(a.board(&["add", "backlog", "next"]).ok(), "b7\n");
    assert_eq!(
        listed(&world, "backlog"),
        ["b1 from b", "b5 by hand", "b6 landed", "b7 next"]
    );
    assert_eq!(
        world
            .remote_log()
            .iter()
            .filter(|s| *s == "board: hand edits")
            .count(),
        1
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
    a.write("proj/chores/c1.md", by_hand("c1", "by hand"));
    a.git(&a.data(), ["add", "proj/chores/c1.md"]);
    fs::remove_file(a.data().join("proj/chores/c1.md")).unwrap();
    a.write("proj/backlog/b9.md", by_hand("b9", "typed"));
    assert_eq!(a.board(&["add", "backlog", "x"]).ok(), "b10\n");
    assert_eq!(world.remote_file("proj/chores/c1.md"), None);
    assert_eq!(listed(&world, "backlog"), ["b9 typed", "b10 x"]);
    in_step(&world, &a);
}

/// Ahead only (a commit by hand, nothing new on the remote): the write
/// pushes it with its own.
#[test]
fn a_clone_ahead_of_the_remote_pushes_its_commits_with_the_write() {
    let world = World::new();
    let a = world.a();
    a.write("shop/backlog/b1.md", by_hand("b1", "by hand"));
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
