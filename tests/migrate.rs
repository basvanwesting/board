//! `board migrate`: a board's old list files converted to item files, tested
//! against what the old tool wrote and printed (`tests/fixtures/migrate`,
//! made by `make.sh` there with a binary of 3a9f1ea, the last list-file
//! version), and the refusal of every other command while a list is there.

mod common;

use board::{Board, Env};
use common::{Machine, World};

const CHORES: &str = include_str!("fixtures/migrate/chores.md");
const BACKLOG: &str = include_str!("fixtures/migrate/backlog.md");
/// The old `ls chores` and `ls backlog`.
const OLD_LS: &str = include_str!("fixtures/migrate/ls.txt");

fn text(bytes: Option<Vec<u8>>) -> String {
    String::from_utf8(bytes.expect("the file is there")).expect("UTF-8")
}

/// A file's text with its `created:` stamp, the migration's time, masked.
fn stamped(bytes: Option<Vec<u8>>) -> String {
    text(bytes)
        .lines()
        .map(|line| match line.strip_prefix("created: ") {
            Some(_) => "created: <now>",
            None => line,
        })
        .map(|line| format!("{line}\n"))
        .collect()
}

/// The fixture lists in `proj`, committed and pushed as the old tool left
/// them.
fn old_board(world: &World) -> Machine<'_> {
    let a = world.a();
    a.write("proj/chores.md", CHORES);
    a.write("proj/backlog.md", BACKLOG);
    a.commit_and_push("the old lists");
    a
}

#[test]
fn the_old_writers_lists_become_item_files_that_list_as_before() {
    let world = World::new();
    let a = old_board(&world);
    let commits = world.remote_log().len();
    assert_eq!(
        a.board(&["migrate"]).ok(),
        "migrated 6 chores, 4 backlog items\n"
    );

    // One commit, pushed: the item files in, both lists out.
    assert_eq!(a.main(), world.remote_main());
    assert_eq!(world.remote_log().len(), commits + 1);
    assert_eq!(
        world.remote_log()[0],
        "proj: migrated 6 chores, 4 backlog items"
    );
    assert_eq!(world.remote_file("proj/chores.md"), None);
    assert_eq!(world.remote_file("proj/backlog.md"), None);
    assert_eq!(a.status(), "");

    // `ls`: the same lines, by id within a group now (the old tool listed
    // by the order its `move`s left), and the backlog under `Default`.
    let ls = a.board(&["ls", "chores"]).ok() + &a.board(&["ls", "backlog"]).ok();
    assert_eq!(
        ls,
        "## Home\n\
         c1 Water the plants. Check: the soil is damp\n\
         c5 Sweep the porch. Check: no leaves\n\
         c6 Call the plumber\n\
         ## Work\n\
         c2 Rotate the key. Check: login works: run it. Done.  (126)\n\
         c3 Ship it! Check: deployed  (c7)\n\
         c4 Is it up? Check: status page: green\n\
         ## Default\n\
         b1 Search by tag  (from 126)\n\
         b2 Export as CSV.  (from b41)\n\
         b3 Dark mode?\n\
         b4 A plain idea  (from c3)\n"
    );
    let mut new: Vec<&str> = ls.lines().filter(|l| *l != "## Default").collect();
    let mut old: Vec<&str> = OLD_LS.lines().collect();
    new.sort_unstable();
    old.sort_unstable();
    assert_eq!(new, old);

    // The files, as the new tool writes them: a title without the join's
    // full stop, the check and the reference as keys, no order key.
    assert_eq!(
        stamped(world.remote_file("proj/chores/c2.md")),
        "id: c2\ngroup: Work\ncheck: login works: run it. Done.\nfrom: 126\n\
         created: <now>\n\n# Rotate the key\n\n\
         Steps:\n  - open the valve\n    - slowly\nthen close it\n"
    );
    assert_eq!(
        stamped(world.remote_file("proj/chores/c1.md")),
        "id: c1\ngroup: Home\ncheck: the soil is damp\ncreated: <now>\n\n\
         # Water the plants\n"
    );
    assert_eq!(
        stamped(world.remote_file("proj/chores/c3.md")),
        "id: c3\ngroup: Work\ncheck: deployed\nfrom: c7\ncreated: <now>\n\n\
         # Ship it!\n"
    );
    assert_eq!(
        stamped(world.remote_file("proj/chores/c6.md")),
        "id: c6\ngroup: Home\ncreated: <now>\n\n# Call the plumber\n"
    );
    assert_eq!(
        stamped(world.remote_file("proj/backlog/b4.md")),
        "id: b4\ngroup: Default\nfrom: c3\ncreated: <now>\n\n# A plain idea\n"
    );
    assert_eq!(
        stamped(world.remote_file("proj/backlog/b2.md")),
        "id: b2\ngroup: Default\nfrom: b41\ncreated: <now>\n\n# Export as CSV.\n\n\
         Why: hard to find.\n  indented once\n"
    );
    assert_eq!(a.board(&["lint"]).ok(), "ok\n");
}

#[test]
fn a_project_without_lists_has_nothing_to_migrate() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "backlog", "one"]).ok();
    let before = world.remote_main();
    assert_eq!(a.board(&["migrate"]).ok(), "nothing to migrate\n");
    assert_eq!(world.remote_main(), before);
}

/// Each line the conversion cannot place: named, and nothing written.
#[test]
fn a_line_it_cannot_place_is_named_and_nothing_is_converted() {
    let item = "id: c1\ngroup: Home\ncreated: x\n\n# Here already\n";
    for (file, list, existing, why) in [
        (
            "chores.md",
            "## Home\n- c1 One. Check: x\nsome notes\n",
            None,
            "proj/chores.md:3: free text: neither an item, a heading, a body line right below an \
             item, nor blank",
        ),
        (
            "chores.md",
            "## Home\n- c1 One. Check: x\n\n  after a blank line\n",
            None,
            "proj/chores.md:4: free text: neither an item, a heading, a body line right below an \
             item, nor blank",
        ),
        (
            "backlog.md",
            "- b1 one\n- an idea without an id\n",
            None,
            "proj/backlog.md:2: an item without an id",
        ),
        (
            "chores.md",
            "- c1 One. Check: x\n## Home\n",
            None,
            "proj/chores.md:1: a chore above the first `## ` heading, so in no group",
        ),
        (
            "backlog.md",
            "## Later\n- b1 one\n",
            None,
            "proj/backlog.md:1: a heading in backlog.md, which has no groups",
        ),
        (
            "backlog.md",
            "- b1 one\n- b2 two\n- b1 again\n",
            None,
            "proj/backlog.md:3: an id an item above has too",
        ),
        (
            "chores.md",
            "## Home\n- c1 One. Check: x\n",
            Some(item),
            "proj/chores.md:2: its id has an item file already",
        ),
        (
            "backlog.md",
            "- b1 one\r\n",
            None,
            "proj/backlog.md:1: a CR line end; convert the file to LF line ends",
        ),
    ] {
        let world = World::new();
        let a = world.a();
        a.write(&format!("proj/{file}"), list);
        if let Some(item) = existing {
            a.write("proj/chores/c1.md", item);
        }
        a.commit_and_push("the old list");
        let before = world.remote_main();
        assert_eq!(
            a.board(&["migrate"]).failed(1),
            format!(
                "board: migrate: nothing converted; fix these lines by hand and run it again:\n  \
                 {why}\n"
            ),
            "{list:?}"
        );
        assert_eq!(world.remote_main(), before, "{list:?}");
        assert_eq!(a.main(), before, "{list:?}");
        assert_eq!(a.status(), "", "{list:?}");
    }
}

/// While a list file is there, the board would look empty or be written
/// next to items it cannot see: every command but `migrate` is refused,
/// reads and the TUI included.
#[test]
fn a_project_with_a_list_file_takes_only_migrate() {
    let world = World::new();
    let a = old_board(&world);
    let refusal = "board: proj/chores.md: a list file of the older storage, which this version \
                   does not read; run `board migrate` to convert it to item files\n";
    let before = world.remote_main();
    for args in [
        &["ls"][..],
        &["ls", "backlog"],
        &["show", "c1"],
        &["lint"],
        &["sync"],
        &["add", "backlog", "x"],
        &["add", "chore", "x", "--group", "Home"],
        &["done", "c1"],
        &["drop", "b1"],
        &["group", "b1", "--to", "Later"],
        &["retitle", "b1", "y"],
        &["new", "brief", "a-brief"],
        &["next"],
    ] {
        assert_eq!(a.board(args).failed(1), refusal, "{args:?}");
    }
    let board = Board::open(
        Env {
            home: a.home(),
            root: a.data(),
            cwd: a.proj(),
            board: None,
        },
        Box::new(|_| {}),
    )
    .expect("the machine's board");
    assert_eq!(
        board::tui::run_headless(board, "q")
            .expect_err("refused")
            .to_string(),
        refusal.strip_prefix("board: ").unwrap().trim_end()
    );
    assert_eq!(world.remote_main(), before);
    assert_eq!(a.status(), "");
}

/// A list that arrives with a merge, as one a machine still on the old
/// binary pushes: the write is refused, and the clone has it.
#[test]
fn a_list_that_comes_in_with_the_merge_is_refused_too() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "backlog", "one"]).ok();
    let b = world.b();
    b.git(&b.data(), ["pull", "-q"]);
    b.write("proj/backlog.md", "- b7 from an old machine\n");
    b.commit_and_push("old binary");
    let ran = a.board(&["add", "backlog", "two"]);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert!(
        ran.stderr.ends_with(
            "board: proj/backlog.md: a list file of the older storage, which this version does \
             not read; run `board migrate` to convert it to item files\n"
        ),
        "{ran:?}"
    );
    assert_eq!(
        a.board(&["migrate"]).ok(),
        "migrated 0 chores, 1 backlog item\n"
    );
    assert_eq!(
        a.board(&["ls", "backlog"]).ok(),
        "## Default\nb1 one\nb7 from an old machine\n"
    );
}

/// After a refusal the owner fixes the list in an editor and runs `migrate`
/// again: the fix is committed first, as any hand edit, and converted. An
/// edit left uncommitted is the version converted, too.
#[test]
fn a_list_fixed_by_hand_after_a_refusal_converts() {
    let world = World::new();
    let a = world.a();
    a.write("proj/chores.md", "# Chores\n## Home\n- c1 One. Check: x\n");
    a.commit_and_push("the old list");
    a.board(&["migrate"]).failed(1);
    a.write(
        "proj/chores.md",
        "## Home\n- c1 One. Check: x\n- c2 Wait... Check: y\n",
    );
    assert_eq!(
        a.board(&["migrate"]).ok(),
        "migrated 2 chores, 0 backlog items\n"
    );
    assert_eq!(
        a.board(&["ls", "chores"]).ok(),
        "## Home\nc1 One. Check: x\nc2 Wait... Check: y\n"
    );
    assert_eq!(world.remote_file("proj/chores.md"), None);
    assert_eq!(a.main(), world.remote_main());
    assert_eq!(a.status(), "");
}

/// A machine still on the old binary committed a list change offline before
/// it was closed, and the migration ran elsewhere: its list is kept over
/// the migration's delete, so the change is refused and named, never lost.
#[test]
fn a_list_changed_offline_before_the_migration_is_kept() {
    let world = World::new();
    let a = world.a();
    a.write("proj/chores.md", "## Home\n- c1 One. Check: x\n");
    a.commit_and_push("the old list");
    let b = world.b();
    b.git(&b.data(), ["pull", "-q"]);
    b.write(
        "proj/chores.md",
        "## Home\n- c1 One. Check: x\n- c3 Added offline\n",
    );
    b.git(&b.data(), ["commit", "-q", "-am", "old binary, offline"]);
    assert_eq!(
        a.board(&["migrate"]).ok(),
        "migrated 1 chore, 0 backlog items\n"
    );
    let refused = b.board(&["migrate"]);
    assert_eq!(refused.code, 1, "{refused:?}");
    assert!(
        refused
            .stderr
            .ends_with("\n  proj/chores.md:2: its id has an item file already\n"),
        "{refused:?}"
    );
    // The owner takes out what is converted already.
    b.write("proj/chores.md", "## Home\n- c3 Added offline\n");
    assert_eq!(
        b.board(&["migrate"]).ok(),
        "migrated 1 chore, 0 backlog items\n"
    );
    assert_eq!(
        b.board(&["ls", "chores"]).ok(),
        "## Home\nc1 One. Check: x\nc3 Added offline\n"
    );
}

/// `migrate` in a project that has item files already: the list's items
/// join them, and the existing files that need nothing stay as they are.
#[test]
fn migrate_folds_a_list_into_the_item_files_there() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "chore", "first", "--group", "Home"]).ok();
    a.board(&["add", "backlog", "one"]).ok();
    let c1 = world.remote_file("proj/chores/c1.md");
    a.write(
        "proj/chores.md",
        "## Home\n- c5 Later. Check: x\n## Away\n- c6 Far\n",
    );
    a.write("proj/backlog.md", "- b4 four\n- b3 three\n");
    a.commit_and_push("came back");
    assert_eq!(
        a.board(&["migrate"]).ok(),
        "migrated 2 chores, 2 backlog items\n"
    );
    assert_eq!(world.remote_file("proj/chores/c1.md"), c1);
    assert_eq!(
        a.board(&["ls", "chores"]).ok(),
        "## Away\nc6 Far\n## Home\nc1 first\nc5 Later. Check: x\n"
    );
    assert_eq!(
        a.board(&["ls", "backlog"]).ok(),
        "## Default\nb1 one\nb3 three\nb4 four\n"
    );
}

/// Item files of an older version of this tool: `migrate` gives each item
/// without a `group:` line `group: Default`, right after `id:`, and takes
/// out every header `order:` line, in one commit; nothing else of a file
/// changes, its body neither, and a file that needs nothing is not in the
/// commit. Run again, it has nothing to do.
#[test]
fn migrate_brings_item_files_up_to_date() {
    let world = World::new();
    let a = world.a();
    let files = [
        // A chore and a backlog item without a group.
        (
            "proj/chores/c1.md",
            "id: c1\norder: 1000\nfrom: 126\ncreated: x\n\n# Water\n",
        ),
        (
            "proj/backlog/b1.md",
            "id: b1\nfrom: c1\norder: 2000\ncreated: x\n\n# Dark mode\n",
        ),
        // A group, a key the tool does not know, a body with key-like lines.
        (
            "proj/backlog/b2.md",
            "id: b2\ngroup: Later\norder: 3000\nowner: me\ncreated: x\n\n# Search\n\n\
             order: 3\ngroup: x\n",
        ),
        // An empty group: rewritten in its place.
        (
            "proj/chores/c3.md",
            "id: c3\nfrom: 126\ngroup:\ncreated: x\n\n# Empty\n",
        ),
        // Nothing to do.
        (
            "proj/chores/c2.md",
            "id: c2\ngroup: Home\ncreated: x\n\n# Sweep\n",
        ),
    ];
    for (file, text) in files {
        a.write(file, text);
    }
    a.commit_and_push("by an older version");
    let commits = world.remote_log().len();
    assert_eq!(a.board(&["migrate"]).ok(), "updated 4 item files\n");
    assert_eq!(world.remote_log().len(), commits + 1);
    assert_eq!(world.remote_log()[0], "proj: updated 4 item files");
    let mut changed = world.remote_changed("main");
    changed.sort();
    assert_eq!(
        changed,
        [
            "proj/backlog/b1.md",
            "proj/backlog/b2.md",
            "proj/chores/c1.md",
            "proj/chores/c3.md"
        ]
    );
    let after = |file: &str| text(world.remote_file(file));
    // An empty `group:` is rewritten where it stands.
    assert_eq!(
        after("proj/chores/c3.md"),
        "id: c3\nfrom: 126\ngroup: Default\ncreated: x\n\n# Empty\n"
    );
    assert_eq!(
        after("proj/chores/c1.md"),
        "id: c1\ngroup: Default\nfrom: 126\ncreated: x\n\n# Water\n"
    );
    assert_eq!(
        after("proj/backlog/b1.md"),
        "id: b1\ngroup: Default\nfrom: c1\ncreated: x\n\n# Dark mode\n"
    );
    assert_eq!(
        after("proj/backlog/b2.md"),
        "id: b2\ngroup: Later\nowner: me\ncreated: x\n\n# Search\n\norder: 3\ngroup: x\n"
    );
    assert_eq!(after("proj/chores/c2.md"), files[4].1);
    assert_eq!(a.board(&["lint"]).ok(), "ok\n");
    let before = world.remote_main();
    assert_eq!(a.board(&["migrate"]).ok(), "nothing to migrate\n");
    assert_eq!(world.remote_main(), before);
    assert_eq!(a.main(), before);
}

/// Lists and item files that need it: one commit does both.
#[test]
fn migrate_converts_the_lists_and_updates_the_item_files_in_one_commit() {
    let world = World::new();
    let a = world.a();
    a.write(
        "proj/backlog/b1.md",
        "id: b1\norder: 1000\ncreated: x\n\n# one\n",
    );
    a.write("proj/chores.md", "## Home\n- c2 Two. Check: x\n");
    a.write("proj/backlog.md", "- b3 three\n");
    a.commit_and_push("both kinds");
    let commits = world.remote_log().len();
    assert_eq!(
        a.board(&["migrate"]).ok(),
        "migrated 1 chore, 1 backlog item; updated 1 item file\n"
    );
    assert_eq!(world.remote_log().len(), commits + 1);
    let mut changed = world.remote_changed("main");
    changed.sort();
    assert_eq!(
        changed,
        [
            "proj/backlog.md",
            "proj/backlog/b1.md",
            "proj/backlog/b3.md",
            "proj/chores.md",
            "proj/chores/c2.md"
        ]
    );
    assert_eq!(
        text(world.remote_file("proj/backlog/b1.md")),
        "id: b1\ngroup: Default\ncreated: x\n\n# one\n"
    );
    assert_eq!(
        stamped(world.remote_file("proj/backlog/b3.md")),
        "id: b3\ngroup: Default\ncreated: <now>\n\n# three\n"
    );
    assert_eq!(a.board(&["migrate"]).ok(), "nothing to migrate\n");
}

/// The refusal is the project's: a write in another project of the clone
/// goes through and leaves the list alone.
#[test]
fn a_write_in_another_project_neither_needs_nor_touches_the_lists() {
    let world = World::new();
    let a = old_board(&world);
    let shop = a.repository("Work/shop");
    let ran = a.board_in(&shop, &["add", "backlog", "elsewhere"]);
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{ran:?}");
    assert_eq!(
        world.remote_file("proj/chores.md").as_deref(),
        Some(CHORES.as_bytes())
    );
    assert_eq!(
        world.remote_file("proj/backlog.md").as_deref(),
        Some(BACKLOG.as_bytes())
    );
    assert_eq!(world.remote_files("shop/backlog"), ["shop/backlog/b1.md"]);
}

/// The next id after the migration is one past the highest the lists ever
/// held, also when that item was deleted before.
#[test]
fn the_next_id_is_past_every_id_the_lists_held() {
    let world = World::new();
    let a = world.a();
    a.write("proj/backlog.md", "- b1 one\n- b9 nine\n");
    a.write("proj/chores.md", "## Home\n- c4 four. Check: x\n");
    a.commit_and_push("lists");
    a.write("proj/backlog.md", "- b1 one\n");
    a.write("proj/chores.md", "## Home\n");
    a.commit_and_push("b9 dropped, c4 done");
    assert_eq!(
        a.board(&["migrate"]).ok(),
        "migrated 0 chores, 1 backlog item\n"
    );
    assert_eq!(a.board(&["add", "backlog", "x"]).ok(), "b10\n");
    assert_eq!(
        a.board(&["add", "chore", "x", "--group", "Home"]).ok(),
        "c5\n"
    );
}
