//! `after:` and `queue --first` through the binary: a brief, a chore or a
//! backlog item waits on another item of the board; `next` skips a brief
//! whose wait is not over, and `--first` puts a queued brief first.

mod common;

use common::{Machine, World};

const SECTIONS: &str = "# A brief\n\n## Task\nt\n## Design\nd\n## Premises\np\n\
                        ## Out of scope\no\n## Verification\nv\n\
                        ## Not doable by the worker\nn\n## Worker protocol\nw\n";

fn text(bytes: Option<Vec<u8>>) -> String {
    String::from_utf8(bytes.expect("the file is there")).unwrap()
}

/// Brief `number`, new and written, still a draft.
#[track_caller]
fn draft(machine: &Machine, number: u32) {
    let path = machine.board(&["new", "brief", "b"]).ok();
    assert!(path.ends_with(&format!("/{number}-b.md\n")), "{path}");
    let file = format!("proj/briefs/{number}-b.md");
    let mut bytes = machine.file(&file).expect("the brief is there");
    bytes.extend_from_slice(SECTIONS.as_bytes());
    machine.write(&file, bytes);
}

/// Brief `number`, new, written and queued with `args` after its number.
#[track_caller]
fn queued(machine: &Machine, number: u32, args: &[&str]) {
    draft(machine, number);
    let id = number.to_string();
    machine.board(&[&["queue", &id][..], args].concat()).ok();
}

#[track_caller]
fn next(machine: &Machine) -> String {
    let said = machine.board(&["next"]).ok();
    match said.rsplit_once('/') {
        Some((_, file)) => file.trim_end().to_owned(),
        None => said.trim_end().to_owned(),
    }
}

#[track_caller]
fn report(machine: &Machine, number: u32) {
    machine
        .board_with_input(&["report", &number.to_string()], b"Done.\n")
        .ok();
}

fn brief_file(world: &World, number: u32) -> String {
    text(world.remote_file(&format!("proj/briefs/{number}-b.md")))
}

// ---- next ----

#[test]
fn next_skips_a_brief_that_waits_until_its_target_is_reported() {
    let world = World::new();
    let a = world.a();
    queued(&a, 1, &[]);
    assert_eq!(next(&a), "1-b.md");
    // 2 is queued before 3, and waits on 1, which is started.
    queued(&a, 2, &["--after", "1"]);
    queued(&a, 3, &[]);
    assert_eq!(
        a.board(&["ls", "briefs"]).ok(),
        "1 started b\n2 queued b  after 1\n3 queued b\n"
    );
    assert_eq!(next(&a), "3-b.md");
    assert_eq!(next(&a), "queue empty, 1 waiting on 1");
    // Reported, not yet filed: the wait is over.
    report(&a, 1);
    assert_eq!(
        a.board(&["ls", "briefs"]).ok(),
        "3 started b\n1 reported b\n2 queued b\n"
    );
    assert_eq!(next(&a), "2-b.md");
}

#[test]
fn a_chore_or_a_backlog_item_as_target_is_waited_on_while_its_file_is_there() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "chore", "Deploy"]).ok();
    queued(&a, 1, &["--after", "c1"]);
    assert_eq!(next(&a), "queue empty, 1 waiting on c1");
    a.board(&["done", "c1"]).ok();
    assert_eq!(next(&a), "1-b.md");

    // A backlog item closed by `file`.
    a.board(&["add", "backlog", "Search"]).ok();
    queued(&a, 2, &["--closes", "b1"]);
    queued(&a, 3, &["--after", "b1"]);
    report(&a, 1);
    a.board(&["file", "1", "--no-commits"]).ok();
    assert_eq!(next(&a), "2-b.md");
    assert_eq!(next(&a), "queue empty, 1 waiting on b1");
    report(&a, 2);
    a.board(&["file", "2", "--no-commits"]).ok();
    assert_eq!(world.remote_files("proj/backlog"), Vec::<String>::new());
    assert_eq!(next(&a), "3-b.md");
}

#[test]
fn a_queue_of_waiting_briefs_says_what_they_wait_on_and_changes_nothing() {
    let world = World::new();
    let a = world.a();
    for chore in ["one", "two"] {
        a.board(&["add", "chore", chore]).ok();
    }
    queued(&a, 1, &["--after", "c2"]);
    queued(&a, 2, &["--after", "c2"]);
    let before = world.remote_main();
    let ran = a.board(&["next"]);
    assert_eq!(
        (ran.code, ran.stdout.as_slice(), ran.stderr.as_str()),
        (0, &b"queue empty, 2 waiting on c2\n"[..], "")
    );
    assert_eq!(world.remote_main(), before);
    // A third, on a draft brief.
    draft(&a, 3);
    queued(&a, 4, &["--after", "3"]);
    assert_eq!(next(&a), "queue empty, 3 waiting on c2, 3");
}

#[test]
fn a_chain_is_taken_link_by_link() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "chore", "Deploy"]).ok();
    queued(&a, 1, &["--after", "c1"]);
    queued(&a, 2, &["--after", "1"]);
    queued(&a, 3, &["--after", "2"]);
    assert_eq!(next(&a), "queue empty, 3 waiting on c1, 1, 2");
    a.board(&["done", "c1"]).ok();
    assert_eq!(next(&a), "1-b.md");
    assert_eq!(next(&a), "queue empty, 2 waiting on 1, 2");
    report(&a, 1);
    assert_eq!(next(&a), "2-b.md");
    report(&a, 2);
    assert_eq!(next(&a), "3-b.md");
}

// ---- queue --first ----

#[test]
fn first_puts_a_queued_brief_first_and_the_latest_wins() {
    let world = World::new();
    let a = world.a();
    for number in 1..=3 {
        queued(&a, number, &[]);
    }
    let queued_at = brief_file(&world, 3)
        .lines()
        .find(|line| line.starts_with("queued: "))
        .unwrap()
        .to_owned();
    assert_eq!(a.board(&["queue", "3", "--first"]).ok(), "3 first\n");
    assert!(brief_file(&world, 3).contains(&format!("\n{queued_at}\n")));
    assert_eq!(
        a.board(&["ls", "briefs"]).ok(),
        "3 queued b\n1 queued b\n2 queued b\n"
    );
    // Two marks with no wait between: the later is first.
    a.board(&["queue", "1", "--first"]).ok();
    a.board(&["queue", "2", "--first"]).ok();
    assert_eq!(next(&a), "2-b.md");
    assert_eq!(next(&a), "1-b.md");
    assert_eq!(next(&a), "3-b.md");
    // Without --first a queued brief is still refused.
    queued(&a, 4, &[]);
    assert_eq!(
        a.board(&["queue", "4"]).failed(1),
        "board: queue 4: it is queued, not draft\n"
    );
}

#[test]
fn a_first_brief_that_waits_is_skipped() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "chore", "Deploy"]).ok();
    queued(&a, 1, &[]);
    queued(&a, 2, &["--after", "c1", "--first"]);
    assert_eq!(
        a.board(&["ls", "briefs"]).ok(),
        "2 queued b  after c1\n1 queued b\n"
    );
    assert_eq!(next(&a), "1-b.md");
}

#[test]
fn first_on_a_draft_queues_it_and_on_a_queued_brief_stands_alone() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "backlog", "Search"]).ok();
    a.board(&["add", "chore", "Deploy"]).ok();
    queued(&a, 1, &[]);
    draft(&a, 2);
    assert_eq!(
        a.board(&["queue", "2", "--first"]).ok(),
        "queued 2, first\n"
    );
    assert!(brief_file(&world, 2).contains("\nfirst: 1\n"));
    draft(&a, 3);
    a.board(&["queue", "3", "--first", "--closes", "b1", "--after", "c1"])
        .ok();
    let three = brief_file(&world, 3);
    assert!(
        three.contains("\ncloses: b1\nafter: c1\nfirst: 2\n"),
        "{three}"
    );
    assert!(three.contains("\nstatus: queued\n"), "{three}");
    let before = world.remote_blobs("proj");
    for args in [
        &["queue", "1", "--first", "--closes", "b1"][..],
        &["queue", "1", "--first", "--after", "c1"],
    ] {
        let said = a.board(args).failed(1);
        assert!(
            said.contains("it is queued, so --first stands alone"),
            "{said}"
        );
    }
    assert_eq!(world.remote_blobs("proj"), before);
}

// ---- refusals ----

#[test]
fn a_wait_on_an_id_never_given_is_refused_and_changes_nothing() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "chore", "Deploy"]).ok();
    a.board(&["add", "backlog", "Search"]).ok();
    draft(&a, 1);
    // The sections written by hand go out first, as with any write.
    a.board(&["sync"]).ok();
    let before = world.remote_blobs("proj");
    let log = world.remote_log();
    for (args, what) in [
        (&["after", "c1", "c9"][..], "after c1"),
        (&["after", "b1", "7"], "after b1"),
        (&["queue", "1", "--after", "b9"], "queue 1"),
        (&["add", "chore", "x", "--after", "c2"], "add chore"),
        (&["add", "backlog", "x", "--after", "2"], "add backlog"),
    ] {
        let said = a.board(args).failed(1);
        assert!(
            said.starts_with(&format!("board: {what}: "))
                && said.ends_with(" was never given on this board, so nothing can wait on it\n"),
            "{args:?}: {said}"
        );
    }
    assert_eq!(world.remote_blobs("proj"), before);
    assert_eq!(world.remote_log(), log);
    assert!(brief_file(&world, 1).contains("\nstatus: draft\n"));
    // No id was given out: the next adds get the next ids.
    assert_eq!(a.board(&["add", "chore", "y"]).ok(), "c2\n");
    assert_eq!(a.board(&["add", "backlog", "y"]).ok(), "b2\n");
}

#[test]
fn a_wait_on_itself_a_loop_or_by_a_started_brief_is_refused() {
    let world = World::new();
    let a = world.a();
    for chore in ["one", "two", "three"] {
        a.board(&["add", "chore", chore]).ok();
    }
    queued(&a, 1, &[]);
    assert_eq!(next(&a), "1-b.md");
    a.board(&["after", "c2", "c1"]).ok();
    a.board(&["after", "c3", "c2"]).ok();
    let before = world.remote_blobs("proj");
    for (args, why) in [
        (
            &["after", "c1", "c1"][..],
            "board: after c1: c1 cannot wait on itself\n",
        ),
        (
            &["after", "c1", "c2"],
            "board: after c1: a loop: c1 → c2 → c1, which would wait for ever\n",
        ),
        (
            &["after", "c1", "c3"],
            "board: after c1: a loop: c1 → c3 → c2 → c1, which would wait for ever\n",
        ),
        (
            &["after", "1", "c1"],
            "board: after 1: it is started; only a draft or a queued brief waits, as a \
             started one no longer waits for anything\n",
        ),
    ] {
        assert_eq!(a.board(args).failed(1), why, "{args:?}");
    }
    assert_eq!(world.remote_blobs("proj"), before);
    // A target that is done already: accepted, and it shows nothing.
    a.board(&["done", "c3"]).ok();
    assert_eq!(a.board(&["after", "c1", "c3"]).ok(), "c1 after c3\n");
    assert_eq!(
        a.board(&["ls", "chores"]).ok(),
        "## Default\nc1 one\nc2 two  after c1\n"
    );
}

#[test]
fn lint_reports_a_wait_on_itself_and_a_loop_made_by_hand() {
    let world = World::new();
    let a = world.a();
    a.write(
        "proj/chores/c1.md",
        "id: c1\ngroup: g\nafter: c1\ncreated: x\n\n# one\n",
    );
    a.write(
        "proj/backlog/b1.md",
        "id: b1\ngroup: g\nafter: b2\ncreated: x\n\n# one\n",
    );
    a.write(
        "proj/backlog/b2.md",
        "id: b2\ngroup: g\nafter: b1\ncreated: x\n\n# two\n",
    );
    a.write(
        "proj/backlog/b3.md",
        "id: b3\ngroup: g\nafter: soon\ncreated: x\n\n# three\n",
    );
    a.write(
        "proj/briefs/1-b.md",
        "id: 1\nstatus: draft\ncloses: b4\nafter: b4\n\n# one\n",
    );
    // An id never given is no finding: it has no file, so holds nothing up.
    a.write(
        "proj/backlog/b4.md",
        "id: b4\ngroup: g\nafter: c99\ncreated: x\n\n# four\n",
    );
    assert_eq!(
        a.board(&["lint"]).out(1),
        "proj/chores/c1.md: after: c1 cannot wait on itself\n\
         proj/backlog/b1.md: after: a loop: b1 → b2 → b1, which would wait for ever\n\
         proj/backlog/b2.md: after: a loop: b2 → b1 → b2, which would wait for ever\n\
         proj/backlog/b3.md: after `soon` is no id like 128, c12 or b41, so it waits on nothing\n\
         proj/briefs/1-b.md: after: 1 closes b4, which goes only when 1 is filed, so it would \
         wait for ever\n"
    );
}

// ---- after, and what shows ----

#[test]
fn after_sets_and_none_removes_the_line_and_nothing_else() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "chore", "Deploy", "--check", "it is up"])
        .ok();
    a.board(&["add", "chore", "Other"]).ok();
    a.board(&["add", "backlog", "Search", "--group", "Later"])
        .ok();
    draft(&a, 1);
    // The sections written by hand go out first, as with any write.
    a.board(&["sync"]).ok();
    queued(&a, 2, &[]);
    let files = [
        ("proj/chores/c1.md", "c1"),
        ("proj/backlog/b1.md", "b1"),
        ("proj/briefs/1-b.md", "1"),
        ("proj/briefs/2-b.md", "2"),
    ];
    let before: Vec<String> = files
        .iter()
        .map(|(f, _)| text(world.remote_file(f)))
        .collect();
    for (_, id) in files {
        assert_eq!(
            a.board(&["after", id, "c2"]).ok(),
            format!("{id} after c2\n")
        );
    }
    let set: Vec<String> = files
        .iter()
        .map(|(f, _)| text(world.remote_file(f)))
        .collect();
    assert!(
        set[0].starts_with("id: c1\ngroup: Default\ncheck: it is up\nafter: c2\ncreated: "),
        "{}",
        set[0]
    );
    assert!(
        set[1].starts_with("id: b1\ngroup: Later\nafter: c2\ncreated: "),
        "{}",
        set[1]
    );
    for (set, before) in set.iter().zip(&before) {
        let without: String = set
            .lines()
            .filter(|line| !line.starts_with("after: "))
            .map(|line| format!("{line}\n"))
            .collect();
        assert_eq!(&without, before);
    }
    for (_, id) in files {
        assert_eq!(
            a.board(&["after", id, "--none"]).ok(),
            format!("{id} after nothing\n")
        );
    }
    let after: Vec<String> = files
        .iter()
        .map(|(f, _)| text(world.remote_file(f)))
        .collect();
    assert_eq!(after, before);
    // Neither or both is a usage error.
    a.board(&["after", "c1"]).failed(2);
    a.board(&["after", "c1", "b1", "--none"]).failed(2);
}

#[test]
fn ls_ends_a_line_with_what_it_waits_on_until_the_wait_is_over() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "chore", "Deploy", "--check", "it is up"])
        .ok();
    a.board(&["add", "chore", "Clean", "--after", "c1"]).ok();
    a.board(&[
        "add", "chore", "Verify", "--check", "it is up", "--after", "c1",
    ])
    .ok();
    a.board(&["add", "backlog", "Search", "--after", "c1", "--from", "c1"])
        .ok();
    queued(&a, 1, &["--after", "c1"]);
    assert_eq!(
        a.board(&[]).ok(),
        "# briefs\n1 queued b  after c1\n\n# chores\n## Default\n\
         c1 Deploy. Check: it is up\nc2 Clean  after c1\nc3 Verify. Check: it is up  after c1\n\n\
         # backlog\n## Default\nb1 Search  (from c1)  after c1\n"
    );
    a.board(&["done", "c1"]).ok();
    assert_eq!(
        a.board(&[]).ok(),
        "# briefs\n1 queued b\n\n# chores\n## Default\nc2 Clean\nc3 Verify. Check: it is up\n\n\
         # backlog\n## Default\nb1 Search  (from c1)\n"
    );
}

/// Queued briefs list in queue order: one marked first, then by `queued:`
/// time, here the reverse of their numbers.
#[test]
fn ls_lists_the_queued_briefs_in_queue_order() {
    let world = World::new();
    let a = world.a();
    for (number, queued, first) in [
        (1, "2026-11-26T10:00:03Z", ""),
        (2, "2026-11-26T10:00:02Z", ""),
        (3, "2026-11-26T10:00:01Z", ""),
        (4, "2026-11-26T10:00:04Z", "first: 1\n"),
    ] {
        a.write(
            &format!("proj/briefs/{number}-b.md"),
            format!("id: {number}\nstatus: queued\n{first}queued: {queued}\n\n{SECTIONS}"),
        );
    }
    a.write(
        "proj/briefs/5-b.md",
        format!("id: 5\nstatus: draft\n\n{SECTIONS}"),
    );
    assert_eq!(
        a.board(&["ls", "briefs"]).ok(),
        "4 queued b\n3 queued b\n2 queued b\n1 queued b\n5 draft b\n"
    );
}

/// A brief that closes a backlog item cannot wait on it: the item goes only
/// when the brief is filed. A wait on a queued brief has its own command.
#[test]
fn a_brief_cannot_wait_on_what_it_closes() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "backlog", "Search"]).ok();
    draft(&a, 1);
    a.board(&["sync"]).ok();
    let before = world.remote_blobs("proj");
    let why = "1 closes b1, which goes only when 1 is filed, so it would wait for ever";
    assert_eq!(
        a.board(&["queue", "1", "--closes", "b1", "--after", "b1"])
            .failed(1),
        format!("board: queue 1: {why}\n")
    );
    assert_eq!(world.remote_blobs("proj"), before);
    a.board(&["queue", "1", "--closes", "b1"]).ok();
    assert_eq!(
        a.board(&["after", "1", "b1"]).failed(1),
        format!("board: after 1: {why}\n")
    );
    // A wait set before, then the `closes` at `queue`.
    a.board(&["add", "backlog", "Export"]).ok();
    draft(&a, 2);
    a.board(&["after", "2", "b2"]).ok();
    assert_eq!(
        a.board(&["queue", "2", "--closes", "b2"]).failed(1),
        "board: queue 2: 2 closes b2, which goes only when 2 is filed, so it would wait for \
         ever\n"
    );
    assert_eq!(
        a.board(&["queue", "1", "--after", "b1"]).failed(1),
        "board: queue 1: it is queued; `board after 1 <id>` changes what it waits on\n"
    );
}

/// `--first` on the brief that is first already, alone: no commit.
#[test]
fn first_on_the_first_brief_changes_nothing() {
    let world = World::new();
    let a = world.a();
    queued(&a, 1, &[]);
    queued(&a, 2, &["--first"]);
    let log = world.remote_log();
    assert_eq!(a.board(&["queue", "2", "--first"]).ok(), "2 first\n");
    assert_eq!(world.remote_log(), log);
    // Not first alone any more (a tie by hand): it is written again.
    a.write(
        "proj/briefs/1-b.md",
        text(a.file("proj/briefs/1-b.md"))
            .replace("status: queued\n", "status: queued\nfirst: 1\n"),
    );
    a.board(&["sync"]).ok();
    let log = world.remote_log();
    a.board(&["queue", "2", "--first"]).ok();
    assert_eq!(world.remote_log().len(), log.len() + 1);
    assert!(brief_file(&world, 2).contains("\nfirst: 2\n"));
    assert_eq!(a.board(&["queue", "2", "--first"]).ok(), "2 first\n");
    assert_eq!(a.board(&["next"]).ok().rsplit('/').next(), Some("2-b.md\n"));
    assert_eq!(
        a.board(&["queue", "2", "--first"]).failed(1),
        "board: queue 2: it is started; --first is for a draft or a queued brief\n"
    );
}
