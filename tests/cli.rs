//! The read side through the binary: `board`, `ls`, `show`, `lint`, and how a
//! command finds its project.

mod common;

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::Path;

use common::{Machine, World, run};

const CHORES: &str = "\
# Chores

## Home
- c1 Water the plants. Check: the soil is wet
  twice a week
- c3 Rotate the key. Check: login works  (126)

## Office

notes between the items
- Added by hand, no id yet. Check: it has an id
";

const BACKLOG: &str = "\
- b2 Faster search  (from 126)
- b1 Dark mode
";

/// A board with a brief in every status, in an order that is not the
/// board's.
fn fill(machine: &Machine) {
    machine.write("proj/chores.md", CHORES);
    machine.write("proj/backlog.md", BACKLOG);
    for (file, status) in [
        ("briefs/127-search.md", "reported"),
        ("briefs/128-export.md", "blocked"),
        ("briefs/129-checkout.md", "started"),
        ("briefs/130-refunds.md", "draft"),
        ("briefs/131-import.md", "queued"),
        ("briefs/done/126-login.md", "filed"),
    ] {
        let number = &file[file.rfind('/').unwrap() + 1..][..3];
        machine.write(
            &format!("proj/{file}"),
            format!("id: {number}\nstatus: {status}\n\n# Brief {number}\n\n## Task\n"),
        );
    }
    machine.write("proj/briefs/done/090-archive.md", "# An old brief\n");
}

const BRIEFS: &str = "\
129 started checkout
128 blocked export
127 reported search
131 queued import
130 draft refunds
";

const CHORE_LINES: &str = "\
## Home
c1 Water the plants. Check: the soil is wet
c3 Rotate the key. Check: login works  (126)
## Office
Added by hand, no id yet. Check: it has an id
";

const BACKLOG_LINES: &str = "\
b2 Faster search  (from 126)
b1 Dark mode
";

// ---- board, ls ----

#[test]
fn a_project_without_a_directory_is_an_empty_board() {
    let world = World::new();
    assert_eq!(
        world.a().board(&[]).ok(),
        "# briefs\n\n# chores\n\n# backlog\n"
    );
    assert_eq!(world.a().board(&["ls", "briefs"]).ok(), "");
    assert_eq!(world.a().board(&["lint"]).ok(), "ok\n");
}

#[test]
fn the_plain_board_is_three_headed_blocks() {
    let world = World::new();
    let a = world.a();
    fill(&a);
    let board = a.board(&[]).ok();
    assert_eq!(
        board,
        format!("# briefs\n{BRIEFS}\n# chores\n{CHORE_LINES}\n# backlog\n{BACKLOG_LINES}")
    );
    assert_eq!(a.board(&["ls"]).ok(), board);
}

#[test]
fn ls_of_one_kind_is_its_block_without_the_head() {
    let world = World::new();
    let a = world.a();
    fill(&a);
    assert_eq!(a.board(&["ls", "briefs"]).ok(), BRIEFS);
    assert_eq!(a.board(&["ls", "chores"]).ok(), CHORE_LINES);
    assert_eq!(a.board(&["ls", "backlog"]).ok(), BACKLOG_LINES);
}

#[test]
fn ls_with_a_status_lists_the_briefs_of_that_status() {
    let world = World::new();
    let a = world.a();
    fill(&a);
    a.write("proj/briefs/132-more.md", "id: 132\nstatus: reported\n");
    let reported = "127 reported search\n132 reported more\n";
    assert_eq!(a.board(&["ls", "--status", "reported"]).ok(), reported);
    assert_eq!(
        a.board(&["ls", "briefs", "--status", "reported"]).ok(),
        reported
    );
    assert_eq!(
        a.board(&["ls", "--status", "draft"]).ok(),
        "130 draft refunds\n"
    );
    // Filed is where the file is; the archived brief has no header to say so.
    assert_eq!(
        a.board(&["ls", "--status", "filed"]).ok(),
        "90 filed archive\n126 filed login\n"
    );
}

#[test]
fn a_brief_without_a_usable_status_is_listed_last_as_unknown() {
    let world = World::new();
    let a = world.a();
    a.write("proj/briefs/1-typo.md", "id: 1\nstatus: strated\n");
    a.write("proj/briefs/2-bare.md", "# No header\n");
    a.write("proj/briefs/3-draft.md", "id: 3\nstatus: draft\n");
    assert_eq!(
        a.board(&["ls", "briefs"]).ok(),
        "3 draft draft\n1 unknown typo\n2 unknown bare\n"
    );
}

#[test]
fn a_status_goes_with_briefs_only() {
    let world = World::new();
    let a = world.a();
    for list in ["chores", "backlog"] {
        assert_eq!(
            a.board(&["ls", list, "--status", "draft"]).failed(2),
            "board: --status is for briefs; chores and backlog items have no status\n"
        );
    }
    let unknown = a.board(&["ls", "--status", "done"]).failed(2);
    assert!(
        unknown.starts_with("board: invalid value 'done' for '--status <STATUS>': `done` is not a status (draft, queued, started, blocked, reported, filed, unpushed)"),
        "{unknown}"
    );
    let no_kind = a.board(&["ls", "ideas"]).failed(2);
    assert!(
        no_kind.starts_with("board: invalid value 'ideas'"),
        "{no_kind}"
    );
}

#[test]
fn a_list_line_is_printed_as_the_file_has_it() {
    let world = World::new();
    let a = world.a();
    // Latin-1, CRLF: not UTF-8, and not the tool's to repair.
    a.write("proj/backlog.md", b"- b1 caf\xe9  \r\n- b2 plain\r\n");
    let ran = a.board(&["ls", "backlog"]);
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));
    assert_eq!(ran.stdout, b"b1 caf\xe9  \nb2 plain\n");
    let ran = a.board(&["show", "b1"]);
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));
    assert_eq!(ran.stdout, b"b1 caf\xe9  \n");
}

// ---- show ----

#[test]
fn show_prints_a_brief_as_the_file_has_it() {
    let world = World::new();
    let a = world.a();
    // No newline at the end, CRLF, a byte that is not UTF-8.
    let open = b"id: 128\r\nstatus: started\r\n\r\n# Export\r\n\r\n## Task\r\ncaf\xe9".to_vec();
    a.write("proj/briefs/128-export.md", &open);
    a.write("proj/briefs/done/007-archive.md", "# An old brief\n");
    let ran = a.board(&["show", "128"]);
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));
    assert_eq!(ran.stdout, open);
    // Open or filed, and the number is a number: `7` finds `007-…`.
    assert_eq!(a.board(&["show", "7"]).ok(), "# An old brief\n");
}

#[test]
fn show_prints_an_item_as_its_line_without_the_dash() {
    let world = World::new();
    let a = world.a();
    fill(&a);
    assert_eq!(
        a.board(&["show", "c3"]).ok(),
        "c3 Rotate the key. Check: login works  (126)\n"
    );
    assert_eq!(a.board(&["show", "b1"]).ok(), "b1 Dark mode\n");
}

/// The indented lines below an item are its own: shown with it, as the file
/// has them, up to the first line that is not one.
#[test]
fn show_prints_an_item_with_its_indented_lines() {
    let world = World::new();
    let a = world.a();
    a.write(
        "proj/backlog.md",
        "- b1 Dark mode\n  why: the night shift\n\t- and a list\n\n  not b1's\n- b2 Other\n",
    );
    assert_eq!(
        a.board(&["show", "b1"]).ok(),
        "b1 Dark mode\n  why: the night shift\n\t- and a list\n"
    );
    assert_eq!(a.board(&["show", "b2"]).ok(), "b2 Other\n");
}

#[test]
fn show_refuses_an_id_that_is_not_there() {
    let world = World::new();
    let a = world.a();
    fill(&a);
    for (id, message) in [
        ("c2", "board: show c2: no such chore\n"),
        ("b3", "board: show b3: no such backlog item\n"),
        ("125", "board: show 125: no such brief\n"),
        // An id is of its list: c1 exists, b1 exists, 1 does not.
        ("1", "board: show 1: no such brief\n"),
    ] {
        assert_eq!(a.board(&["show", id]).failed(1), message);
    }
}

#[test]
fn show_refuses_an_id_that_names_two_things() {
    let world = World::new();
    let a = world.a();
    a.write(
        "proj/chores.md",
        "- c20 one. Check: x\n- c20 two. Check: x\n- c21 three. Check: x\n",
    );
    a.write("proj/backlog.md", "- b5 one\n- b5 two\n- b5 three\n");
    a.write("proj/briefs/128-export.md", "id: 128\nstatus: draft\n");
    a.write("proj/briefs/done/128-import.md", "id: 128\nstatus: filed\n");
    assert_eq!(
        a.board(&["show", "c20"]).failed(1),
        "board: show c20: c20 is on 2 lines; fix the duplicate first\n"
    );
    assert_eq!(
        a.board(&["show", "b5"]).failed(1),
        "board: show b5: b5 is on 3 lines; fix the duplicate first\n"
    );
    assert_eq!(
        a.board(&["show", "128"]).failed(1),
        "board: show 128: 128 is on 2 files; fix the duplicate first\n"
    );
    assert_eq!(a.board(&["show", "c21"]).ok(), "c21 three. Check: x\n");
}

#[test]
fn what_is_no_id_is_a_usage_error() {
    let world = World::new();
    let a = world.a();
    for word in ["c012", "x1", "128-export", ""] {
        let said = a.board(&["show", word]).failed(2);
        assert!(
            said.starts_with(&format!(
                "board: invalid value '{word}' for '<ID>': `{word}` is not an id like 128, c12 or b41"
            )),
            "{said}"
        );
    }
    let missing = a.board(&["show"]).failed(2);
    assert!(missing.starts_with("board: "), "{missing}");
    assert!(missing.contains("<ID>"), "{missing}");
}

// ---- lint ----

#[test]
fn lint_says_ok_when_there_is_nothing_to_fix() {
    let world = World::new();
    let a = world.a();
    fill(&a);
    assert_eq!(a.board(&["lint"]).ok(), "ok\n");
}

#[test]
fn lint_prints_a_line_per_finding_and_exits_1() {
    let world = World::new();
    let a = world.a();
    a.write(
        "proj/chores.md",
        "## Home\n- c1 Water the plants\n- c2 Rotate the key. Check: login works\n- c2 Again. Check: x\n",
    );
    a.write("proj/backlog.md", "- b1 one\n\n- b1 two\n");
    a.write("proj/briefs/127-search.md", "id: 127\nstatus: strated\n");
    a.write("proj/briefs/128-export.md", "id: 129\nstatus: draft\n");
    a.write("proj/briefs/129-a.md", "id: 129\nstatus: draft\n");
    a.write("proj/briefs/done/129-b.md", "# Archived, no header\n");
    a.write("proj/briefs/130-bare.md", "# No header\n");
    a.write("proj/briefs/notes.md", "not a brief\n");
    a.write("proj/briefs/sent/126-old.md", "an old folder\n");
    a.write(
        "proj/briefs/done/README.txt",
        "not markdown, not the board's\n",
    );
    assert_eq!(
        a.board(&["lint"]).out(1),
        "\
proj/chores.md:2: chore without a check; say after `Check:` how to see that it is done
proj/chores.md:4: duplicate id c2, first on line 3; delete the id on one of the lines and that line gets a new one
proj/backlog.md:3: duplicate id b1, first on line 1; delete the id on one of the lines and that line gets a new one
proj/briefs/127-search.md: unknown status `strated`; a brief outside briefs/done/ is draft, queued, started, blocked or reported
proj/briefs/128-export.md: header id `129` differs from the file name's 128
proj/briefs/done/129-b.md: duplicate brief number 129, also proj/briefs/129-a.md; give one of the two another number
proj/briefs/130-bare.md: no `status:` line; a brief outside briefs/done/ is draft, queued, started, blocked or reported
proj/briefs/notes.md: not at briefs/[done/]<number>-<slug>.md, so it is not a brief and not synced
proj/briefs/sent/126-old.md: not at briefs/[done/]<number>-<slug>.md, so it is not a brief and not synced
"
    );
}

#[test]
fn lint_is_about_this_project_only() {
    let world = World::new();
    let a = world.a();
    a.write("shop/chores.md", "- c1 No check\n");
    assert_eq!(a.board(&["lint"]).ok(), "ok\n");
    assert_eq!(
        a.board(&["--board", "shop", "lint"]).out(1),
        "shop/chores.md:1: chore without a check; say after `Check:` how to see that it is done\n"
    );
}

// ---- which project ----

#[test]
fn the_project_is_the_repository_s_path_below_work() {
    let world = World::new();
    let a = world.a();
    a.write("proj/backlog.md", "- b1 of proj\n");
    a.write("tools-shop/backlog.md", "- b1 of tools-shop\n");

    // From anywhere inside the repository.
    let inside = a.proj().join("src").join("deep");
    fs::create_dir_all(&inside).unwrap();
    assert_eq!(a.board_in(&inside, &["show", "b1"]).ok(), "b1 of proj\n");

    // A repository deeper below Work: `/` becomes `-`.
    let nested = a.repository("Work/tools/shop");
    assert_eq!(
        a.board_in(&nested, &["show", "b1"]).ok(),
        "b1 of tools-shop\n"
    );

    // The checkout names the project, wherever its git directory is kept (a
    // submodule's is inside the repository it is part of).
    let kept_elsewhere = a.home().join("Work").join("tools").join("till");
    let git_directories = a.home().join("modules");
    fs::create_dir(&git_directories).unwrap();
    a.git(
        &git_directories,
        [
            OsStr::new("init"),
            "-q".as_ref(),
            "--separate-git-dir".as_ref(),
            "till".as_ref(),
            kept_elsewhere.as_ref(),
        ],
    );
    a.write("tools-till/backlog.md", "- b1 of tools-till\n");
    assert_eq!(
        a.board_in(&kept_elsewhere, &["show", "b1"]).ok(),
        "b1 of tools-till\n"
    );
}

/// A second checkout of a repository that has a commit, as `git worktree
/// add` makes it.
fn add_worktree(machine: &Machine, repository: &Path, worktree: &Path) {
    machine.git(
        repository,
        [
            OsStr::new("worktree"),
            "add".as_ref(),
            "-q".as_ref(),
            "--detach".as_ref(),
            worktree.as_ref(),
        ],
    );
}

/// `git worktree add` gives a project a second checkout, at a path of that
/// session's choosing. Each of these paths is a project name of its own, so
/// the board they would give is a wrong one that looks right: empty.
#[test]
fn a_linked_worktree_is_on_the_board_of_its_repository() {
    let world = World::new();
    let a = world.a();
    a.write("proj/backlog.md", "- b1 of proj\n");
    a.write(
        "proj-.claude-worktrees-x/backlog.md",
        "- b1 of the worktree's path\n",
    );
    a.write("proj-wt/backlog.md", "- b1 of the worktree's path\n");

    for place in [
        "Work/proj/.claude/worktrees/x",
        "Work/proj-wt",
        // Where the worktree is does not matter, not even being below Work.
        "elsewhere/proj-wt",
    ] {
        let worktree = a.home().join(place);
        add_worktree(&a, &a.proj(), &worktree);
        assert_eq!(
            a.board_in(&worktree, &["show", "b1"]).ok(),
            "b1 of proj\n",
            "{place}"
        );
        let inside = worktree.join("src");
        fs::create_dir(&inside).unwrap();
        assert_eq!(
            a.board_in(&inside, &["show", "b1"]).ok(),
            "b1 of proj\n",
            "{place}"
        );
    }
    // The repository's own checkout still is what it was.
    assert_eq!(a.board(&["show", "b1"]).ok(), "b1 of proj\n");
}

#[test]
fn a_linked_worktree_of_a_repository_that_gives_no_project_is_a_usage_error() {
    let world = World::new();
    let a = world.a();
    let work = a.home().join("Work");

    // Below ~/Work itself, but its repository is not: the worktree's own
    // path (`shop-wt`) names no board.
    let outside = a.repository("elsewhere/shop");
    a.git(&outside, ["commit", "-q", "--allow-empty", "-m", "start"]);
    let worktree = work.join("shop-wt");
    add_worktree(&a, &outside, &worktree);
    assert_eq!(
        a.board_in(&worktree, &["ls"]).failed(2),
        format!(
            "board: no project for {}, the repository of the worktree {}: it is not below {}; \
             name the board with --board <name>\n",
            outside.display(),
            worktree.display(),
            work.display()
        )
    );

    // A repository without a checkout of its own has no path that names it.
    let bare = work.join("bare.git");
    a.git(
        &work,
        [
            OsStr::new("clone"),
            "-q".as_ref(),
            "--bare".as_ref(),
            a.proj().as_ref(),
            bare.as_ref(),
        ],
    );
    let worktree = work.join("bare-wt");
    add_worktree(&a, &bare, &worktree);
    assert_eq!(
        a.board_in(&worktree, &["ls"]).failed(2),
        format!(
            "board: no project for {}: it is a worktree of {}, which is not the .git of a checkout; \
             name the board with --board <name>\n",
            worktree.display(),
            bare.display()
        )
    );
    // Named, it has a board like any other directory.
    assert_eq!(a.board_in(&worktree, &["--board", "proj", "ls"]).code, 0);
}

/// The board-data clone is a repository below `~/Work` too, and the place
/// where lists are edited by hand. Its path names a project nobody has, so
/// the board there would be a wrong one that looks right: empty, lint `ok`.
#[test]
fn the_board_data_clone_gives_no_project() {
    let world = World::new();
    let a = world.a();
    a.write("shop/chores.md", "- c1 No check\n");
    let data = a.data();
    let refused = |shown: String| {
        format!(
            "board: no project for {shown}: it is the board-data clone, which holds every \
             project's board; name the board with --board <name>\n"
        )
    };

    for directory in [data.clone(), data.join("shop")] {
        for command in [&["ls"][..], &["lint"], &[]] {
            assert_eq!(
                a.board_in(&directory, command).failed(2),
                refused(data.display().to_string()),
                "{} {command:?}",
                directory.display()
            );
        }
        // Named, the board is read from there like from anywhere.
        assert_eq!(
            a.board_in(&directory, &["--board", "shop", "lint"]).out(1),
            "shop/chores.md:1: chore without a check; say after `Check:` how to see that it is done\n"
        );
    }

    // A worktree added to the clone is on the clone's board, which is none.
    a.git(&data, ["add", "shop"]);
    a.git(&data, ["commit", "-q", "-m", "shop"]);
    let worktree = a.home().join("Work").join("data-wt");
    add_worktree(&a, &data, &worktree);
    assert_eq!(
        a.board_in(&worktree, &["ls"]).failed(2),
        refused(format!(
            "{}, the repository of the worktree {}",
            data.display(),
            worktree.display()
        ))
    );

    // It is the clone that is refused, not its name: with the board kept
    // elsewhere, a repository at ~/Work/board-data is a project like any
    // other, and the clone is refused where it is, reached through a
    // symlink or not.
    let kept_elsewhere = a.repository("Work/kept");
    fs::create_dir(kept_elsewhere.join("board-data")).unwrap();
    fs::write(
        kept_elsewhere.join("board-data").join("backlog.md"),
        "- b1 of board-data\n",
    )
    .unwrap();
    let link = a.home().join("link-to-kept");
    std::os::unix::fs::symlink(&kept_elsewhere, &link).unwrap();
    for root in [&kept_elsewhere, &link] {
        assert_eq!(
            run(a
                .command()
                .env("BOARD_ROOT", root)
                .current_dir(&data)
                .args(["show", "b1"]))
            .ok(),
            "b1 of board-data\n"
        );
        assert_eq!(
            run(a
                .command()
                .env("BOARD_ROOT", root)
                .current_dir(&kept_elsewhere)
                .arg("ls"))
            .failed(2),
            refused(kept_elsewhere.display().to_string())
        );
    }
}

/// A home that is reached through a symlink (a home on another disk, `/tmp`
/// on a Mac): git names the repository by where it really is, so `~/Work`
/// has to be taken the same way or no project is below it.
#[test]
fn a_symlink_on_the_way_to_work_does_not_move_the_project() {
    let world = World::new();
    let a = world.a();
    a.write("proj/backlog.md", "- b1 of proj\n");
    let link = a.home().with_file_name("link-to-a");
    std::os::unix::fs::symlink(a.home(), &link).unwrap();
    let through_link = link.join("Work").join("proj");

    // The home, the directory, and both.
    assert_eq!(
        run(a.command().env("HOME", &link).args(["show", "b1"])).ok(),
        "b1 of proj\n"
    );
    assert_eq!(
        a.board_in(&through_link, &["show", "b1"]).ok(),
        "b1 of proj\n"
    );
    assert_eq!(
        run(a
            .command()
            .env("HOME", &link)
            .current_dir(&through_link)
            .args(["show", "b1"]))
        .ok(),
        "b1 of proj\n"
    );
    // And what is outside Work stays outside.
    let outside = a.repository("elsewhere/shop");
    assert_eq!(
        run(a
            .command()
            .env("HOME", &link)
            .current_dir(&outside)
            .arg("ls"))
        .failed(2),
        format!(
            "board: no project for {}: it is not below {}; name the board with --board <name>\n",
            outside.display(),
            a.home().join("Work").display()
        )
    );
}

#[test]
fn a_given_board_replaces_the_derived_one() {
    let world = World::new();
    let a = world.a();
    a.write("proj/backlog.md", "- b1 of proj\n");
    a.write("shop/backlog.md", "- b1 of shop\n");
    a.write("other/backlog.md", "- b1 of other\n");
    let show = |command: &mut std::process::Command| run(command.args(["show", "b1"])).ok();

    assert_eq!(show(a.command().args(["--board", "shop"])), "b1 of shop\n");
    assert_eq!(
        a.board(&["show", "b1", "--board", "shop"]).ok(),
        "b1 of shop\n"
    );
    assert_eq!(show(a.command().env("BOARD", "shop")), "b1 of shop\n");
    // The argument is nearer than the environment.
    assert_eq!(
        show(a.command().env("BOARD", "shop").args(["--board", "other"])),
        "b1 of other\n"
    );
    // Empty is unset.
    assert_eq!(show(a.command().env("BOARD", "")), "b1 of proj\n");
}

#[test]
fn a_given_board_works_where_no_project_can_be_derived() {
    let world = World::new();
    let a = world.a();
    a.write("shop/backlog.md", "- b1 of shop\n");
    assert_eq!(
        a.board_in(&a.home(), &["--board", "shop", "show", "b1"])
            .ok(),
        "b1 of shop\n"
    );
}

#[test]
fn a_directory_that_gives_no_project_is_a_usage_error_naming_board() {
    let world = World::new();
    let a = world.a();
    let home = a.home();
    let work = home.join("Work");

    // Not in a repository. The middle is git's own sentence, which differs
    // between versions and file systems.
    let said = a.board_in(&home, &["ls"]).failed(2);
    let start = format!(
        "board: no project for {}: fatal: not a git repository",
        home.display()
    );
    assert!(said.starts_with(&start), "{said}");
    assert!(
        said.ends_with("; name the board with --board <name>\n"),
        "{said}"
    );
    // A repository, but not below ~/Work.
    let outside = a.repository("elsewhere/shop");
    assert_eq!(
        a.board_in(&outside, &["ls"]).failed(2),
        format!(
            "board: no project for {}: it is not below {}; name the board with --board <name>\n",
            outside.display(),
            work.display()
        )
    );
    // Below ~/Work, but the path is no project name.
    let spaced = a.repository("Work/my shop");
    assert_eq!(
        a.board_in(&spaced, &["ls"]).failed(2),
        format!(
            "board: `my shop` (from {}) is not a project name ([A-Za-z0-9][A-Za-z0-9._-]*); \
             name the board with --board <name>\n",
            spaced.display()
        )
    );
}

#[test]
fn a_given_board_has_to_be_a_project_name() {
    let world = World::new();
    let a = world.a();
    for name in ["", "my shop", "../shop", "shop/briefs", ".git"] {
        assert_eq!(
            a.board(&["--board", name, "ls"]).failed(2),
            format!(
                "board: `{name}` (from --board or BOARD) is not a project name ([A-Za-z0-9][A-Za-z0-9._-]*)\n"
            )
        );
    }
    assert_eq!(
        run(a.command().env("BOARD", "../shop").arg("ls")).failed(2),
        "board: `../shop` (from --board or BOARD) is not a project name ([A-Za-z0-9][A-Za-z0-9._-]*)\n"
    );
}

/// `board` is also started from inside git (a hook, `rebase --exec`), with
/// git's variables pointing at whatever repository that git is busy with.
#[test]
fn git_s_own_variables_do_not_move_the_project() {
    let world = World::new();
    let a = world.a();
    a.write("proj/backlog.md", "- b1 of proj\n");
    let inside = a.proj().join("src");
    fs::create_dir_all(&inside).unwrap();
    let ran = run(a
        .command()
        .current_dir(&inside)
        .env("GIT_DIR", a.data().join(".git"))
        .env("GIT_WORK_TREE", a.data())
        .env(
            "GIT_INDEX_FILE",
            a.data().join(".git").join("no-such-index"),
        )
        .env("GIT_PREFIX", "proj/")
        .args(["show", "b1"]));
    assert_eq!(ran.ok(), "b1 of proj\n");
}

// ---- where the board is ----

#[test]
fn board_root_names_the_clone() {
    let world = World::new();
    let a = world.a();
    let b = world.b();
    b.write("proj/backlog.md", "- b1 in the other clone\n");
    assert_eq!(a.board(&["ls", "backlog"]).ok(), "");
    assert_eq!(
        run(a
            .command()
            .env("BOARD_ROOT", b.data())
            .args(["ls", "backlog"]))
        .ok(),
        "b1 in the other clone\n"
    );
    // Empty is unset.
    assert_eq!(
        run(a.command().env("BOARD_ROOT", "").args(["ls", "backlog"])).ok(),
        ""
    );
}

#[test]
fn a_clone_that_is_not_there_is_not_an_empty_board() {
    let world = World::new();
    let a = world.a();
    let gone = a.home().join("Work").join("no-clone");
    for command in [&["ls"][..], &["lint"], &["show", "c1"]] {
        assert_eq!(
            run(a.command().env("BOARD_ROOT", &gone).args(command)).failed(4),
            format!(
                "board: no board-data clone at {gone}; clone it there once (git clone <url> {gone}), \
                 or point BOARD_ROOT at it\n",
                gone = gone.display()
            )
        );
    }
}

/// What a read shows is the files of this clone, as they are now: saved by
/// hand, not committed, not on the other machine.
#[test]
fn a_read_shows_the_files_of_its_own_clone() {
    let world = World::new();
    world
        .a()
        .write("proj/backlog.md", "- b1 saved a moment ago\n");
    assert_eq!(
        world.a().board(&["ls", "backlog"]).ok(),
        "b1 saved a moment ago\n"
    );
    assert_eq!(world.b().board(&["ls", "backlog"]).ok(), "");
}

/// A read runs no git in the board-data clone, so with the board named it
/// runs no git at all: it works where there is none.
#[test]
fn a_read_of_a_named_board_runs_no_git() {
    let world = World::new();
    let a = world.a();
    fill(&a);
    let no_programs = a.home().join("empty");
    fs::create_dir(&no_programs).unwrap();
    let board = |args: &[&str]| run(a.command().env("PATH", &no_programs).args(args));

    assert_eq!(board(&["--board", "proj", "ls", "briefs"]).ok(), BRIEFS);
    assert_eq!(
        board(&["--board", "proj", "show", "b1"]).ok(),
        "b1 Dark mode\n"
    );
    assert_eq!(board(&["--board", "proj", "lint"]).ok(), "ok\n");
    // Deriving the project is the one thing that needs it.
    let said = board(&["ls"]).failed(4);
    assert!(said.starts_with("board: cannot run git: "), "{said}");
}

// ---- output ----

/// `board ls | head -1`: the reader is gone before the output is written.
/// The read end is closed before the process starts, so every write to
/// stdout fails with a closed pipe, without a race.
#[test]
fn a_closed_stdout_is_a_quiet_success() {
    let world = World::new();
    let a = world.a();
    fill(&a);
    a.write(
        "proj/briefs/notes.md",
        "a lint finding, so lint would exit 1\n",
    );
    assert_eq!(a.board(&["lint"]).code, 1);
    for args in [
        &[][..],
        &["ls"],
        &["show", "127"],
        &["show", "c1"],
        &["lint"],
        &["--help"],
    ] {
        let (reader, writer) = io::pipe().unwrap();
        drop(reader);
        let ran = run(a.command().args(args).stdout(writer));
        assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "board {args:?}");
    }
}

#[test]
fn an_unknown_command_is_a_usage_error() {
    let world = World::new();
    let said = world.a().board(&["frobnicate"]).failed(2);
    assert!(
        said.starts_with("board: unrecognized subcommand 'frobnicate'"),
        "{said}"
    );
    assert!(said.contains("Usage: board"), "{said}");
}

#[test]
fn help_goes_to_stdout_and_names_the_commands() {
    let world = World::new();
    let help = world.a().board(&["--help"]).ok();
    for command in ["ls", "show", "lint", "--board"] {
        assert!(help.contains(command), "{command} is not in:\n{help}");
    }
}

// ---- the `unpushed` label ----

/// The project repository with a bare upstream it has pushed to, and a
/// commit per message on top, not pushed.
fn upstream(machine: &Machine, world: &World, messages: &[&str]) {
    let bare = world.dir().join("proj-upstream.git");
    machine.git(
        world.dir(),
        ["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
    );
    machine.git(
        &machine.proj(),
        ["remote", "add", "origin", bare.to_str().unwrap()],
    );
    machine.git(&machine.proj(), ["push", "-q", "-u", "origin", "main"]);
    for message in messages {
        commit(machine, message);
    }
}

fn commit(machine: &Machine, message: &str) {
    machine.git(
        &machine.proj(),
        ["commit", "-q", "--allow-empty", "-m", message],
    );
}

fn push(machine: &Machine) {
    machine.git(&machine.proj(), ["push", "-q"]);
}

/// Filed briefs 1, 12 and 120, reported 3, queued 4.
fn briefs(machine: &Machine) {
    for (file, status) in [
        ("briefs/done/1-one.md", "filed"),
        ("briefs/done/12-twelve.md", "filed"),
        ("briefs/done/120-more.md", "filed"),
        ("briefs/3-read-me.md", "reported"),
        ("briefs/4-next.md", "queued"),
    ] {
        machine.write(&format!("proj/{file}"), format!("status: {status}\n"));
    }
}

/// A filed brief with a commit above the project's upstream is listed
/// with the open ones as `unpushed`, after reported and before queued,
/// until the commit is pushed; the trailer names its brief by the whole
/// number.
#[test]
fn a_filed_brief_with_unpushed_commits_is_listed_as_unpushed() {
    let world = World::new();
    let a = world.a();
    briefs(&a);
    upstream(&a, &world, &["one\n\nBrief: 1", "twelve\n\nBrief: 12"]);
    push(&a);
    // Brief 1: one commit pushed, one not.
    commit(&a, "one more\n\nBrief: 1  ");
    let ls = |args: &[&str]| a.board(&[&["ls", "briefs"], args].concat()).ok();
    assert_eq!(
        ls(&[]),
        "3 reported read-me\n1 unpushed one\n4 queued next\n"
    );
    assert_eq!(ls(&["--status", "unpushed"]), "1 unpushed one\n");
    assert_eq!(
        ls(&["--status", "filed"]),
        "12 filed twelve\n120 filed more\n"
    );
    assert!(
        a.board(&["ls"])
            .ok()
            .starts_with("# briefs\n3 reported read-me\n1 unpushed one\n")
    );
    // Amended: a new hash, the same trailer.
    a.git(
        &a.proj(),
        [
            "commit",
            "-q",
            "--amend",
            "--allow-empty",
            "-m",
            "amended\n\nBrief: 1",
        ],
    );
    assert_eq!(ls(&["--status", "unpushed"]), "1 unpushed one\n");
    push(&a);
    assert_eq!(ls(&[]), "3 reported read-me\n4 queued next\n");
    assert_eq!(ls(&["--status", "unpushed"]), "");
    assert_eq!(
        ls(&["--status", "filed"]),
        "1 filed one\n12 filed twelve\n120 filed more\n"
    );
    // A log encoding with NUL bytes in it does not split the messages.
    a.git(&a.proj(), ["config", "i18n.logOutputEncoding", "UTF-16"]);
    commit(&a, "again\n\nBrief: 12");
    assert_eq!(ls(&["--status", "unpushed"]), "12 unpushed twelve\n");
}

/// Where the checkout says nothing about the project's pushes, nothing is
/// labelled and nothing is refused: no upstream, a detached `HEAD`, or a
/// checkout of another project with `--board`.
#[test]
fn without_an_upstream_of_the_project_nothing_is_labelled() {
    let world = World::new();
    let a = world.a();
    briefs(&a);
    upstream(&a, &world, &["one\n\nBrief: 1"]);
    let open = "3 reported read-me\n4 queued next\n";
    let labelled = "3 reported read-me\n1 unpushed one\n4 queued next\n";
    assert_eq!(a.board(&["ls", "briefs"]).ok(), labelled);
    // The same project named: labelled; another checkout naming it: not.
    assert_eq!(a.board(&["ls", "briefs", "--board", "proj"]).ok(), labelled);
    // Another project's checkout, with a commit of its own for its own
    // brief 1, not pushed: it says nothing about this board's brief 1.
    let other = a.repository("Work/other");
    let bare = world.dir().join("other-upstream.git");
    a.git(
        world.dir(),
        ["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
    );
    a.git(&other, ["commit", "-q", "--allow-empty", "-m", "start"]);
    a.git(&other, ["remote", "add", "origin", bare.to_str().unwrap()]);
    a.git(&other, ["push", "-q", "-u", "origin", "main"]);
    a.git(
        &other,
        ["commit", "-q", "--allow-empty", "-m", "theirs\n\nBrief: 1"],
    );
    // Its own board would label its brief 1: the rule is the checkout's.
    a.write("other/briefs/done/1-theirs.md", "status: filed\n");
    assert_eq!(
        a.board_in(&other, &["ls", "briefs"]).ok(),
        "1 unpushed theirs\n"
    );
    let ran = a.board_in(&other, &["ls", "briefs", "--board", "proj"]);
    assert_eq!((ran.code, ran.stdout.as_slice()), (0, open.as_bytes()));
    a.git(&a.proj(), ["checkout", "-q", "--detach"]);
    assert_eq!(a.board(&["ls", "briefs"]).ok(), open);
    a.git(&a.proj(), ["checkout", "-q", "main"]);
    a.git(&a.proj(), ["branch", "-q", "--unset-upstream"]);
    assert_eq!(a.board(&["ls", "briefs"]).ok(), open);
    // A branch with no commit yet whose upstream was fetched: `@{upstream}`
    // names a commit, `HEAD` none.
    let fresh = a.repository("Work/fresh");
    let upstream = world.dir().join("proj-upstream.git");
    a.git(
        &fresh,
        ["remote", "add", "origin", upstream.to_str().unwrap()],
    );
    a.git(&fresh, ["fetch", "-q", "origin"]);
    a.git(&fresh, ["config", "branch.main.remote", "origin"]);
    a.git(&fresh, ["config", "branch.main.merge", "refs/heads/main"]);
    a.write("fresh/briefs/done/1-one.md", "status: filed\n");
    assert_eq!(a.board_in(&fresh, &["ls", "briefs"]).ok(), "");
}
