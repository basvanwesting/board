//! Parked briefs through the binary: `park` puts a finished draft in
//! `briefs/parked/`, where `next` never looks; `queue` releases one, `queue
//! --parked` all; `file --not-built` files a brief that built nothing.

mod common;

use common::{Machine, World};

const SECTIONS: &str = "# A brief\n\n## Task\nt\n## Design\nd\n## Premises\np\n\
                        ## Out of scope\no\n## Verification\nv\n\
                        ## Not doable by the worker\nn\n## Worker protocol\nw\n";

fn text(bytes: Option<Vec<u8>>) -> String {
    String::from_utf8(bytes.expect("the file is there")).unwrap()
}

/// Brief `number`, new and written, still a draft, and synced.
#[track_caller]
fn draft(machine: &Machine, number: u32) {
    let path = machine.board(&["new", "brief", "b"]).ok();
    assert!(path.ends_with(&format!("/{number}-b.md\n")), "{path}");
    let file = format!("proj/briefs/{number}-b.md");
    let mut bytes = machine.file(&file).expect("the brief is there");
    bytes.extend_from_slice(SECTIONS.as_bytes());
    machine.write(&file, bytes);
    machine.board(&["sync"]).ok();
}

#[track_caller]
fn parked(machine: &Machine, number: u32, args: &[&str]) {
    draft(machine, number);
    let id = number.to_string();
    assert_eq!(
        machine.board(&[&["park", &id][..], args].concat()).ok(),
        format!("parked {number}\n")
    );
}

#[track_caller]
fn next(machine: &Machine) -> String {
    let said = machine.board(&["next"]).ok();
    match said.rsplit_once('/') {
        Some((_, file)) => file.trim_end().to_owned(),
        None => said.trim_end().to_owned(),
    }
}

fn header(file: &str, key: &str) -> Option<String> {
    file.lines()
        .take_while(|line| !line.is_empty())
        .find_map(|line| line.strip_prefix(&format!("{key}: ")).map(str::to_owned))
}

#[test]
fn park_moves_a_written_draft_to_the_parked_folder_in_one_commit() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "backlog", "Fix the typo"]).ok();
    a.board(&["add", "chore", "Deploy"]).ok();
    draft(&a, 1);
    let before = text(world.remote_file("proj/briefs/1-b.md"));
    let commits = world.remote_log().len();
    assert_eq!(
        a.board(&["park", "1", "--closes", "b1", "--after", "c1"])
            .ok(),
        "parked 1\n"
    );
    assert_eq!(world.remote_log().len(), commits + 1);
    assert_eq!(world.remote_file("proj/briefs/1-b.md"), None);
    assert_eq!(
        world.remote_files("proj/briefs/parked"),
        ["proj/briefs/parked/1-b.md"]
    );
    let file = text(world.remote_file("proj/briefs/parked/1-b.md"));
    assert_eq!(header(&file, "status").as_deref(), Some("parked"));
    assert!(
        header(&file, "parked").is_some_and(|at| at.ends_with('Z')),
        "{file}"
    );
    assert_eq!(header(&file, "closes").as_deref(), Some("b1"));
    assert_eq!(header(&file, "after").as_deref(), Some("c1"));
    // The rest byte for byte.
    let rest = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|line| {
                !["status: ", "parked: ", "closes: ", "after: "]
                    .iter()
                    .any(|key| line.starts_with(key))
            })
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(rest(&file), rest(&before));
}

#[test]
fn park_refuses_what_queue_refuses_and_changes_nothing() {
    let world = World::new();
    let a = world.a();
    draft(&a, 1);
    a.board(&["queue", "1"]).ok();
    // An empty draft: no sections.
    a.board(&["new", "brief", "empty"]).ok();
    draft(&a, 3);
    let files = world.remote_blobs("proj");
    for (args, why) in [
        (
            &["park", "1"][..],
            "board: park 1: it is queued, not draft\n",
        ),
        (
            &["park", "2"],
            "board: park 2: no text under ## Task, ## Design, ## Premises, ## Out of scope, \
             ## Verification, ## Not doable by the worker, ## Worker protocol; every one of \
             Task, Design, Premises, Out of scope, Verification, Not doable by the worker, \
             Worker protocol needs a line\n",
        ),
        (
            &["park", "3", "--after", "3"],
            "board: park 3: 3 cannot wait on itself\n",
        ),
        (
            &["park", "3", "--closes", "b9"],
            "board: park 3: closes b9: no such backlog item\n",
        ),
    ] {
        assert_eq!(a.board(args).failed(1), why, "{args:?}");
    }
    let said = a.board(&["park", "3", "--after", "c9"]).failed(1);
    assert!(said.contains("c9 was never given on this board"), "{said}");
    assert_eq!(world.remote_blobs("proj"), files);
}

#[test]
fn next_never_takes_a_parked_brief() {
    let world = World::new();
    let a = world.a();
    parked(&a, 1, &[]);
    let before = world.remote_main();
    assert_eq!(next(&a), "queue empty");
    assert_eq!(world.remote_main(), before);
    draft(&a, 2);
    a.board(&["queue", "2"]).ok();
    assert_eq!(next(&a), "2-b.md");
    assert_eq!(next(&a), "queue empty");
}

#[test]
fn queue_releases_a_parked_brief_and_keeps_its_parked_line() {
    let world = World::new();
    let a = world.a();
    parked(&a, 1, &[]);
    parked(&a, 2, &[]);
    draft(&a, 3);
    a.board(&["queue", "3"]).ok();
    assert_eq!(a.board(&["queue", "1"]).ok(), "queued 1\n");
    assert_eq!(world.remote_file("proj/briefs/parked/1-b.md"), None);
    let file = text(world.remote_file("proj/briefs/1-b.md"));
    assert_eq!(header(&file, "status").as_deref(), Some("queued"));
    assert!(header(&file, "parked").is_some() && header(&file, "queued").is_some());
    // --first: before 3, queued earlier.
    assert_eq!(
        a.board(&["queue", "2", "--first"]).ok(),
        "queued 2, first\n"
    );
    assert_eq!(next(&a), "2-b.md");
    // 3 and 1 by their `queued:` time, then by number: in one second, 1.
    let mut rest = [next(&a), next(&a)];
    rest.sort();
    assert_eq!(rest, ["1-b.md", "3-b.md"]);
    // --closes and --after were set at `park`: refused, nothing moved.
    a.board(&["add", "backlog", "x"]).ok();
    parked(&a, 4, &[]);
    let files = world.remote_blobs("proj");
    for args in [
        &["queue", "4", "--closes", "b1"][..],
        &["queue", "4", "--after", "b1"],
    ] {
        let said = a.board(args).failed(1);
        assert!(said.starts_with("board: queue 4: it is parked"), "{said}");
    }
    assert_eq!(world.remote_blobs("proj"), files);
}

#[test]
fn queue_parked_releases_all_in_one_commit() {
    let world = World::new();
    let a = world.a();
    for number in 1..=3 {
        parked(&a, number, &[]);
    }
    draft(&a, 4);
    let commits = world.remote_log().len();
    assert_eq!(a.board(&["queue", "--parked"]).ok(), "queued 1, 2, 3\n");
    assert_eq!(world.remote_log().len(), commits + 1);
    assert_eq!(
        world.remote_files("proj/briefs/parked"),
        Vec::<String>::new()
    );
    let queued_at = |n: u32| {
        header(
            &text(world.remote_file(&format!("proj/briefs/{n}-b.md"))),
            "queued",
        )
    };
    assert_eq!(queued_at(1), queued_at(3));
    assert_eq!(next(&a), "1-b.md");
    assert_eq!(next(&a), "2-b.md");
    assert_eq!(next(&a), "3-b.md");
    assert!(text(world.remote_file("proj/briefs/4-b.md")).contains("\nstatus: draft\n"));
    // None parked: said so, no commit.
    let log = world.remote_log();
    assert_eq!(a.board(&["queue", "--parked"]).ok(), "nothing parked\n");
    assert_eq!(world.remote_log(), log);
    for args in [
        &["queue", "4", "--parked"][..],
        &["queue", "--parked", "--first"],
        &["queue", "--parked", "--closes", "b1"],
        &["queue"],
    ] {
        let said = a.board(args).failed(2);
        assert!(said.starts_with("board: "), "{args:?}: {said}");
    }
}

#[test]
fn a_parked_brief_is_out_of_the_lists_and_of_the_worker_s_commands() {
    let world = World::new();
    let a = world.a();
    parked(&a, 1, &[]);
    draft(&a, 2);
    for (args, why) in [
        (
            &["amend", "1"][..],
            "board: amend 1: it is parked, not queued, started or blocked\n",
        ),
        (
            &["report", "1"],
            "board: report 1: it is parked, not started\n",
        ),
        (
            &["file", "1", "--no-commits"],
            "board: file 1: it is parked, not reported\n",
        ),
    ] {
        assert_eq!(a.board_with_input(args, b"x").failed(1), why, "{args:?}");
    }
    assert_eq!(
        a.board(&["block", "1", "why?"]).failed(1),
        "board: block 1: it is parked, not started\n"
    );
    assert_eq!(
        a.board(&["show", "1"]).ok(),
        text(a.file("proj/briefs/parked/1-b.md"))
    );
    assert_eq!(a.board(&["ls", "briefs"]).ok(), "2 draft b\n");
    assert!(!a.board(&["ls"]).ok().contains("parked"));
    assert!(!a.board(&[]).ok().contains("parked"));
    assert_eq!(
        a.board(&["ls", "briefs", "--status", "parked"]).ok(),
        "1 parked b\n"
    );
    // `after` changes its wait and nothing else.
    a.board(&["add", "chore", "Deploy"]).ok();
    let before = text(world.remote_file("proj/briefs/parked/1-b.md"));
    assert_eq!(a.board(&["after", "1", "c1"]).ok(), "1 after c1\n");
    let set = text(world.remote_file("proj/briefs/parked/1-b.md"));
    let without: String = set
        .lines()
        .filter(|line| !line.starts_with("after: "))
        .map(|line| format!("{line}\n"))
        .collect();
    assert_eq!(without, before);
    a.board(&["after", "1", "--none"]).ok();
    assert_eq!(text(world.remote_file("proj/briefs/parked/1-b.md")), before);
}

#[test]
fn a_queued_brief_waiting_on_a_parked_one_is_not_claimed() {
    let world = World::new();
    let a = world.a();
    parked(&a, 1, &[]);
    draft(&a, 2);
    a.board(&["queue", "2", "--after", "1"]).ok();
    assert_eq!(next(&a), "queue empty, 1 waiting on 1");
}

#[test]
fn file_not_built_keeps_what_the_brief_closes() {
    let world = World::new();
    let a = world.a();
    for item in ["one", "two"] {
        a.board(&["add", "backlog", item]).ok();
    }
    for (number, closes) in [(1, "b1"), (2, "b2")] {
        draft(&a, number);
        a.board(&["queue", &number.to_string(), "--closes", closes])
            .ok();
        a.board(&["next"]).ok();
        a.board_with_input(&["report", &number.to_string()], b"Not built: p failed.\n")
            .ok();
    }
    assert_eq!(
        a.board(&["file", "1", "--not-built"]).ok(),
        "filed 1 (not built)\n"
    );
    assert!(world.remote_file("proj/backlog/b1.md").is_some());
    assert!(world.remote_file("proj/briefs/done/1-b.md").is_some());
    assert_eq!(
        a.board(&["file", "2", "--no-commits"]).ok(),
        "filed 2 (no commits) closed b2\n"
    );
    assert_eq!(world.remote_file("proj/backlog/b2.md"), None);
    let said = a
        .board(&["file", "2", "--no-commits", "--not-built"])
        .failed(2);
    assert!(said.starts_with("board: "), "{said}");
}

#[test]
fn lint_goes_by_the_place_of_a_parked_brief() {
    let world = World::new();
    let a = world.a();
    a.write(
        "proj/briefs/parked/1-x.md",
        format!("id: 1\nstatus: draft\n\n{SECTIONS}"),
    );
    a.write(
        "proj/briefs/2-y.md",
        format!("id: 2\nstatus: parked\n\n{SECTIONS}"),
    );
    a.write("proj/briefs/parked/3-z.md", "id: 3\nstatus: parked\n");
    a.write("proj/briefs/done/3-z.md", "id: 3\nstatus: filed\n");
    a.write("proj/briefs/parked/notes.md", "not a brief\n");
    assert_eq!(
        a.board(&["ls", "briefs", "--status", "parked"]).ok(),
        "1 parked x\n3 parked z\n"
    );
    assert_eq!(
        a.board(&["lint"]).out(1),
        "proj/briefs/2-y.md: unknown status `parked`; a brief outside briefs/parked/ and \
         briefs/done/ is draft, queued, started, blocked or reported\n\
         proj/briefs/done/3-z.md: duplicate brief number 3, also proj/briefs/parked/3-z.md; \
         give one of the two another number\n\
         proj/briefs/parked/notes.md: not named as the board names its files \
         (briefs/[parked/|done/]<number>-<slug>.md, chores/c<id>.md, backlog/b<id>.md), so it \
         is not synced\n"
    );
}

/// A brief parked on one clone and edited by hand on the other: one file
/// for its number after both sync; the clone whose version lost keeps a
/// copy, as for a brief filed on one side and changed on the other.
#[test]
fn a_park_met_by_a_hand_edit_on_the_other_clone_keeps_one_file() {
    let world = World::new();
    let a = world.a();
    draft(&a, 1);
    let b = world.b();
    b.board(&["sync"]).ok();
    a.board(&["park", "1"]).ok();
    let file = "proj/briefs/1-b.md";
    let edited = text(b.file(file)).replace("## Task\nt\n", "## Task\nt, edited\n");
    b.write(file, edited);
    let ran = b.board(&["sync"]);
    assert_eq!(ran.code, 0, "{ran:?}");
    a.board(&["sync"]).ok();
    for machine in [&a, &b] {
        let briefs: Vec<String> = ["proj/briefs/1-b.md", "proj/briefs/parked/1-b.md"]
            .into_iter()
            .filter(|path| machine.file(path).is_some())
            .map(str::to_owned)
            .collect();
        assert_eq!(briefs, ["proj/briefs/parked/1-b.md"]);
    }
    assert!(ran.stderr.contains("board: conflict: brief 1"), "{ran:?}");
    assert!(b.file("proj/briefs/1-b.md.local").is_some());
    assert!(
        b.board(&["lint"])
            .out(1)
            .contains("unmerged local version of brief 1")
    );
}

/// The place decides the release too: a file placed in `briefs/parked/` by
/// hand with another `status:` is queued from it; one with an empty
/// section is refused, at `queue <id>` and for the whole of `queue
/// --parked`, which then releases none.
#[test]
fn a_release_goes_by_the_place_and_asks_for_a_written_brief() {
    let world = World::new();
    let a = world.a();
    a.write(
        "proj/briefs/parked/1-x.md",
        format!("id: 1\nstatus: queued\n\n{SECTIONS}"),
    );
    a.write("proj/briefs/parked/2-y.md", "id: 2\n\n# no sections\n");
    a.board(&["sync"]).ok();
    assert_eq!(a.board(&["queue", "1"]).ok(), "queued 1\n");
    assert!(text(world.remote_file("proj/briefs/1-x.md")).contains("\nstatus: queued\n"));
    let files = world.remote_blobs("proj");
    let said = a.board(&["queue", "2"]).failed(1);
    assert!(
        said.starts_with("board: queue 2: no text under ## Task"),
        "{said}"
    );
    a.write(
        "proj/briefs/parked/3-z.md",
        format!("id: 3\nstatus: parked\n\n{SECTIONS}"),
    );
    a.board(&["sync"]).ok();
    let files_with_3 = world.remote_blobs("proj");
    assert_ne!(files, files_with_3);
    let said = a.board(&["queue", "--parked"]).failed(1);
    assert!(
        said.starts_with("board: queue --parked: 2: no text under"),
        "{said}"
    );
    assert_eq!(world.remote_blobs("proj"), files_with_3);
}

/// Released on one clone while the other edits it by hand in
/// `briefs/parked/`: the remote's stands, and the other clone keeps its
/// edit as a copy, which lint names.
#[test]
fn a_release_met_by_a_hand_edit_of_the_parked_file_keeps_a_copy() {
    let world = World::new();
    let a = world.a();
    parked(&a, 1, &[]);
    let b = world.b();
    b.board(&["sync"]).ok();
    a.board(&["queue", "1"]).ok();
    let file = "proj/briefs/parked/1-b.md";
    let edited = text(b.file(file)).replace("## Task\nt\n", "## Task\nt, edited\n");
    b.write(file, edited);
    let ran = b.board(&["sync"]);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert!(ran.stderr.contains("board: conflict: brief 1"), "{ran:?}");
    assert_eq!(b.file(file), None);
    assert!(text(b.file("proj/briefs/1-b.md")).contains("\nstatus: queued\n"));
    assert!(b.file("proj/briefs/parked/1-b.md.local").is_some());
    assert!(
        b.board(&["lint"])
            .out(1)
            .contains("unmerged local version of brief 1")
    );
}
