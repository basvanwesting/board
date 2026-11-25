//! The brief commands through the binary: `new brief`, `queue`, `next`,
//! `block`, `amend`, `report`, `file`, on two clones of one bare remote.

mod common;

use std::fs;

use common::{Machine, World, pre_push};

/// What the planner writes below the header: text in every required
/// section.
const SECTIONS: &str = "# Fix the login\n\n## Task\nt\n## Design\nd\n## Premises\np\n\
                        ## Out of scope\no\n## Verification\nv\n\
                        ## Not doable by the worker\nn\n## Worker protocol\nw\n";

fn text(bytes: Option<Vec<u8>>) -> String {
    String::from_utf8(bytes.expect("the file is there")).unwrap()
}

/// The clone is where the remote is, with nothing changed by hand.
#[track_caller]
fn in_step(world: &World, machine: &Machine) {
    assert_eq!(machine.main(), world.remote_main());
    assert_eq!(machine.status(), "");
}

/// `board new brief <slug>`; gives the path it printed.
#[track_caller]
fn new_brief(machine: &Machine, slug: &str) -> String {
    let out = machine.board(&["new", "brief", slug]).ok();
    out.strip_suffix('\n').expect("one line").to_owned()
}

/// Writes the planner's sections below a brief's header, by hand.
fn write_sections(machine: &Machine, file: &str) {
    let mut bytes = machine.file(file).expect("the brief is there");
    bytes.extend_from_slice(SECTIONS.as_bytes());
    machine.write(file, bytes);
}

/// A brief that is new, written and queued.
fn queued(machine: &Machine, number: u32, slug: &str) {
    new_brief(machine, slug);
    write_sections(machine, &format!("proj/briefs/{number}-{slug}.md"));
    assert_eq!(
        machine.board(&["queue", &number.to_string()]).ok(),
        format!("queued {number}\n")
    );
}

/// The value of a header line of a brief on the remote.
fn header(world: &World, file: &str, key: &str) -> Option<String> {
    text(world.remote_file(file))
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key}: ")).map(str::to_owned))
}

#[track_caller]
fn a_stamp(value: Option<String>) {
    let value = value.expect("a stamp");
    assert!(
        value.len() == 20 && value.starts_with("20") && value.ends_with('Z'),
        "{value}"
    );
}

// ---- new brief ----

#[test]
fn new_brief_prints_its_path_and_pushes_its_header() {
    let world = World::new();
    let a = world.a();
    let path = new_brief(&a, "fix-login");
    assert_eq!(
        path,
        a.data()
            .join("proj/briefs/1-fix-login.md")
            .to_string_lossy()
    );
    let file = text(world.remote_file("proj/briefs/1-fix-login.md"));
    let lines: Vec<&str> = file.split('\n').collect();
    assert_eq!(
        lines[..3],
        [
            "id: 1",
            "status: draft",
            &format!("base: {}", a.head_of(&a.proj()))
        ]
    );
    a_stamp(lines[3].strip_prefix("created: ").map(str::to_owned));
    assert_eq!(lines[4..], ["", ""]);
    assert_eq!(fs::read_to_string(&path).unwrap(), file);
    assert_eq!(world.remote_log(), ["proj: new brief 1-fix-login", "start"]);
    in_step(&world, &a);

    assert_eq!(a.board(&["ls", "briefs"]).ok(), "1 draft fix-login\n");
    assert!(new_brief(&a, "second").ends_with("/proj/briefs/2-second.md"));
}

#[test]
fn a_slug_is_lowercase_words_joined_by_dashes() {
    let world = World::new();
    let a = world.a();
    for slug in ["Fix", "fix--login", "fix-", "fix login", "fix/login", ""] {
        let said = a.board(&["new", "brief", slug]).failed(2);
        assert!(said.contains("is not a slug"), "{slug}: {said}");
    }
    assert_eq!(world.remote_log(), ["start"]);
}

/// The base is the `HEAD` of the repository the command runs in, also when
/// `--board` names the project.
#[test]
fn the_base_is_the_head_of_the_repository_the_command_runs_in() {
    let world = World::new();
    let a = world.a();
    let other = a.repository("Work/other");
    a.git(&other, ["commit", "-q", "--allow-empty", "-m", "other"]);
    let path = a
        .board_in(&other, &["--board", "proj", "new", "brief", "x"])
        .ok();
    assert!(path.ends_with("/proj/briefs/1-x.md\n"), "{path}");
    assert_eq!(
        header(&world, "proj/briefs/1-x.md", "base"),
        Some(a.head_of(&other))
    );

    let empty = a.repository("Work/empty");
    assert_eq!(
        a.board_in(&empty, &["--board", "proj", "new", "brief", "y"])
            .failed(1),
        format!(
            "board: new brief: the repository at {} has no commit to base the brief on\n",
            empty.display()
        )
    );
    let said = a
        .board_in(&a.home(), &["--board", "proj", "new", "brief", "y"])
        .failed(2);
    assert!(
        said.starts_with(&format!(
            "board: new brief: no repository at {} to base the brief on",
            a.home().display()
        )),
        "{said}"
    );
    assert_eq!(world.remote_log(), ["proj: new brief 1-x", "start"]);
}

#[test]
fn new_brief_needs_the_remote() {
    let world = World::new();
    let a = world.a();
    a.set_online(false);
    let said = a.board(&["new", "brief", "x"]).failed(3);
    assert!(
        said.starts_with("board: new brief: needs the remote, which was not reached ("),
        "{said}"
    );
    assert_eq!(a.file("proj/briefs/1-x.md"), None);
    assert_eq!(world.remote_log(), ["start"]);
}

/// A brief's number is one past every brief the project ever had: filed
/// ones, ones deleted by hand, and ones added and deleted on a merged side
/// branch, which plain `git log -- <dir>` would not show.
#[test]
fn a_brief_number_is_never_given_twice() {
    let world = World::new();
    let (a, b) = (world.a(), world.b());
    // An archive placed by hand, with leading zeros.
    a.write("proj/briefs/done/007-old.md", "# Old\n");
    assert!(new_brief(&a, "next").ends_with("/8-next.md"));
    // Deleted by hand.
    fs::remove_file(a.data().join("proj/briefs/8-next.md")).unwrap();
    assert!(new_brief(&a, "after-delete").ends_with("/9-after-delete.md"));

    // B adds brief 20 and deletes it again while offline, then merges.
    b.board(&["sync"]).ok();
    b.set_online(false);
    b.write("proj/briefs/20-side.md", "status: draft\n");
    b.board(&["sync"]).failed(3);
    fs::remove_file(b.data().join("proj/briefs/20-side.md")).unwrap();
    b.board(&["sync"]).failed(3);
    new_brief(&a, "meanwhile");
    b.set_online(true);
    let ran = b.board(&["sync"]);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert!(
        ran.stderr.starts_with("board: merged 2 local commits"),
        "{ran:?}"
    );
    assert!(new_brief(&a, "past-the-side").ends_with("/21-past-the-side.md"));
}

/// A board-data repository whose root commit already held briefs (an
/// imported archive) counts them as given, also with `log.showRoot` off,
/// which leaves the root commit's files out of `git log`.
#[test]
fn a_brief_of_the_root_commit_counts_whatever_log_show_root_says() {
    let world = World::new();
    let a = world.a();
    a.git(&a.data(), ["checkout", "-q", "--orphan", "import"]);
    a.write("proj/briefs/done/5-old.md", "# Old\n");
    a.git(&a.data(), ["add", "-A"]);
    a.git(&a.data(), ["commit", "-q", "-m", "import"]);
    a.git(&a.data(), ["push", "-q", "-f", "origin", "import:main"]);
    a.git(&a.data(), ["checkout", "-q", "-B", "main", "import"]);
    a.git(&a.data(), ["branch", "-q", "-D", "import"]);
    a.git(&a.data(), ["config", "log.showRoot", "false"]);
    fs::remove_file(a.data().join("proj/briefs/done/5-old.md")).unwrap();
    a.commit_and_push("delete by hand");
    assert!(new_brief(&a, "next").ends_with("/proj/briefs/6-next.md"));
    in_step(&world, &a);
}

// ---- queue ----

#[test]
fn queue_stamps_the_draft_and_writes_what_it_closes() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "backlog", "one"]).ok();
    a.board(&["add", "backlog", "two"]).ok();
    new_brief(&a, "x");
    write_sections(&a, "proj/briefs/1-x.md");
    assert_eq!(
        a.board(&["queue", "1", "--closes", "b2,b1"]).ok(),
        "queued 1\n"
    );
    let file = "proj/briefs/1-x.md";
    assert_eq!(header(&world, file, "status").as_deref(), Some("queued"));
    assert_eq!(header(&world, file, "closes").as_deref(), Some("b2, b1"));
    a_stamp(header(&world, file, "queued"));
    assert!(text(world.remote_file(file)).ends_with(SECTIONS));
    // The sections written by hand are committed first.
    assert_eq!(
        world.remote_log()[..2],
        ["proj: queued 1", "board: hand edits"]
    );
    in_step(&world, &a);
}

#[test]
fn queue_refuses_what_is_not_a_written_draft() {
    let world = World::new();
    let a = world.a();
    new_brief(&a, "x");
    assert_eq!(
        a.board(&["queue", "1"]).failed(1),
        "board: queue 1: no text under ## Task, ## Design, ## Premises, ## Out of scope, \
         ## Verification, ## Not doable by the worker, ## Worker protocol; every one of Task, \
         Design, Premises, Out of scope, Verification, Not doable by the worker, Worker protocol \
         needs a line\n"
    );
    write_sections(&a, "proj/briefs/1-x.md");
    assert_eq!(
        a.board(&["queue", "1", "--closes", "b4"]).failed(1),
        "board: queue 1: closes b4: no such backlog item\n"
    );
    a.board(&["queue", "1"]).ok();
    assert_eq!(
        a.board(&["queue", "1"]).failed(1),
        "board: queue 1: it is queued, not draft\n"
    );
    assert_eq!(
        a.board(&["queue", "2"]).failed(1),
        "board: queue 2: no such brief\n"
    );
    for args in [
        &["queue", "b1"][..],
        &["queue", "1", "--closes", "c1"],
        &["queue"],
    ] {
        a.board(args).failed(2);
    }
    assert_eq!(world.remote_log()[0], "proj: queued 1");
}

/// Offline a queue is committed on the clone, and the next write that
/// reaches the remote pushes it.
#[test]
fn queue_offline_waits_on_the_clone() {
    let world = World::new();
    let a = world.a();
    new_brief(&a, "x");
    write_sections(&a, "proj/briefs/1-x.md");
    a.set_online(false);
    let ran = a.board(&["queue", "1"]);
    assert_eq!((ran.code, ran.stdout.as_slice()), (0, &b"queued 1\n"[..]));
    assert!(
        ran.stderr.starts_with("board: not pushed: 2 commits ("),
        "{ran:?}"
    );
    assert_eq!(
        header(&world, "proj/briefs/1-x.md", "status").as_deref(),
        Some("draft")
    );
    a.set_online(true);
    a.board(&["sync"]).ok();
    assert_eq!(
        header(&world, "proj/briefs/1-x.md", "status").as_deref(),
        Some("queued")
    );
    in_step(&world, &a);
}

// ---- next ----

#[test]
fn next_claims_the_briefs_in_the_order_they_were_queued() {
    let world = World::new();
    let (a, b) = (world.a(), world.b());
    // Brief 2 is queued first, by hand with an earlier time.
    queued(&a, 1, "one");
    new_brief(&a, "two");
    a.write(
        "proj/briefs/2-two.md",
        format!("id: 2\nstatus: queued\nqueued: 2000-01-01T00:00:00Z\n\n{SECTIONS}"),
    );
    a.board(&["sync"]).ok();

    let path = b.board(&["next"]).ok();
    assert_eq!(
        path,
        format!("{}\n", b.data().join("proj/briefs/2-two.md").display())
    );
    assert_eq!(
        header(&world, "proj/briefs/2-two.md", "status").as_deref(),
        Some("started")
    );
    a_stamp(header(&world, "proj/briefs/2-two.md", "started"));
    assert_eq!(
        fs::read(path.trim_end()).unwrap(),
        world.remote_file("proj/briefs/2-two.md").unwrap()
    );
    assert!(b.board(&["next"]).ok().ends_with("/proj/briefs/1-one.md\n"));

    // An empty queue commits nothing.
    let log = world.remote_log();
    assert_eq!(b.board(&["next"]).ok(), "queue empty\n");
    assert_eq!(world.remote_log(), log);
    assert_eq!(log[..2], ["proj: started 1", "proj: started 2"]);
    in_step(&world, &b);
    assert_eq!(
        b.board(&["ls", "briefs"]).ok(),
        "1 started one\n2 started two\n"
    );
}

/// A claim is shared like an id: offline `next` is refused, an empty queue
/// too, because the queue the clone sees may be stale.
#[test]
fn next_needs_the_remote() {
    let world = World::new();
    let a = world.a();
    queued(&a, 1, "one");
    a.set_online(false);
    let said = a.board(&["next"]).failed(3);
    assert!(
        said.starts_with("board: next: needs the remote, which was not reached ("),
        "{said}"
    );
    assert_eq!(
        header(&world, "proj/briefs/1-one.md", "status").as_deref(),
        Some("queued")
    );
    in_step(&world, &a);
}

/// The push of a claim reached the remote, but `board` heard that it failed:
/// the claim is not made again on the next brief.
#[test]
fn a_claim_that_landed_unheard_is_not_made_twice() {
    let world = World::new();
    let a = world.a();
    queued(&a, 1, "one");
    queued(&a, 2, "two");
    a.hook_once("pre-push", pre_push::LOST_ACK);
    let ran = a.board(&["next"]);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert!(
        String::from_utf8(ran.stdout)
            .unwrap()
            .ends_with("/proj/briefs/1-one.md\n")
    );
    assert!(!a.has_hook("pre-push"));
    assert_eq!(
        header(&world, "proj/briefs/1-one.md", "status").as_deref(),
        Some("started")
    );
    assert_eq!(
        header(&world, "proj/briefs/2-two.md", "status").as_deref(),
        Some("queued")
    );
    assert_eq!(
        world
            .remote_log()
            .iter()
            .filter(|s| s.starts_with("proj: started"))
            .count(),
        1
    );
    in_step(&world, &a);
}

/// Two clones claiming at once get different briefs: B claims while A's push
/// is on its way, and A claims again on what B pushed.
///
/// Both claims are made within the same second, short of a second turning
/// in the milliseconds between them: the two commits are then the same but
/// for the run mark in their messages, and without it A would take B's push
/// for its own and print brief 1 too.
#[test]
fn two_clones_claiming_at_once_get_different_briefs() {
    let world = World::new();
    let (a, b) = (world.a(), world.b());
    queued(&a, 1, "one");
    queued(&a, 2, "two");
    b.board(&["sync"]).ok();
    a.hook_once(
        "pre-push",
        &pre_push::window(&b.script(&["next"]), world.dir()),
    );
    let ran = a.board(&["next"]);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert!(ran.stderr.contains("board: retry 2/8"), "{ran:?}");
    let window = |name: &str| fs::read_to_string(world.dir().join(name)).unwrap();
    assert_eq!(window("window.code"), "0\n");
    assert!(window("window.out").ends_with("/proj/briefs/1-one.md\n"));
    let printed = String::from_utf8(ran.stdout).unwrap();
    assert!(printed.ends_with("/proj/briefs/2-two.md\n"), "{printed}");
    for file in ["proj/briefs/1-one.md", "proj/briefs/2-two.md"] {
        assert_eq!(header(&world, file, "status").as_deref(), Some("started"));
    }
    in_step(&world, &a);
}

// ---- block, amend, report ----

/// A brief that is new, written, queued and claimed by `next`.
fn started(machine: &Machine, number: u32, slug: &str) {
    queued(machine, number, slug);
    let path = machine.board(&["next"]).ok();
    assert!(path.ends_with(&format!("/{number}-{slug}.md\n")), "{path}");
}

/// The lines of a brief on the remote after `from` (a line of it), with
/// every stamp written as `<T>`.
fn after(world: &World, file: &str, from: &str) -> String {
    let text = text(world.remote_file(file));
    let at = text
        .find(from)
        .unwrap_or_else(|| panic!("{from} in {text}"));
    text[at..]
        .lines()
        .map(|line| match line.strip_prefix("### ") {
            Some(stamp) if stamp.len() == 20 && stamp.ends_with('Z') => "### <T>\n".to_owned(),
            _ => format!("{line}\n"),
        })
        .collect()
}

#[test]
fn a_brief_is_blocked_amended_and_reported() {
    let world = World::new();
    let (a, b) = (world.a(), world.b());
    started(&a, 1, "x");
    let file = "proj/briefs/1-x.md";

    assert_eq!(a.board(&["block", "1", "Which db?"]).ok(), "blocked 1\n");
    assert_eq!(header(&world, file, "status").as_deref(), Some("blocked"));
    a_stamp(header(&world, file, "blocked"));
    assert_eq!(a.board(&["ls", "briefs"]).ok(), "1 blocked x\n");

    // The planner answers on the other clone.
    let ran = b.board_with_input(&["amend", "1"], b"\nUse sqlite.\n\n");
    assert_eq!(ran.out(0), "amended 1, resumed\n");
    assert_eq!(header(&world, file, "status").as_deref(), Some("started"));
    assert_eq!(
        b.board_with_input(&["amend", "1"], b"Also: ```code```\n")
            .ok(),
        "amended 1\n"
    );

    let report = "Ran: cargo test\n## Checks\nall green\n";
    assert_eq!(
        a.board_with_input(&["report", "1"], report.as_bytes()).ok(),
        "reported 1\n"
    );
    assert_eq!(header(&world, file, "status").as_deref(), Some("reported"));
    a_stamp(header(&world, file, "reported"));
    assert_eq!(
        after(&world, file, "## Worker protocol\n"),
        "## Worker protocol\nw\n\
         ## Questions\n\n### <T>\n\nWhich db?\n\
         ## Amendments\n\n### <T>\n\nUse sqlite.\n\n### <T>\n\nAlso: ```code```\n\
         ## Report\nRan: cargo test\n## Checks\nall green\n"
    );
    // `started:` is the first start; a resume is none.
    assert_eq!(
        text(world.remote_file(file))
            .lines()
            .filter(|line| line.starts_with("started: "))
            .count(),
        1
    );
    assert_eq!(
        world.remote_log()[..4],
        [
            "proj: reported 1",
            "proj: amended 1",
            "proj: amended 1",
            "proj: blocked 1"
        ]
    );
    in_step(&world, &a);
}

#[test]
fn block_amend_and_report_refuse_what_does_not_fit() {
    let world = World::new();
    let a = world.a();
    queued(&a, 1, "x");
    let log = world.remote_log();
    assert_eq!(
        a.board(&["block", "1", "q"]).failed(1),
        "board: block 1: it is queued, not started\n"
    );
    assert_eq!(
        a.board_with_input(&["report", "1"], b"r").failed(1),
        "board: report 1: it is queued, not started\n"
    );
    assert_eq!(
        a.board_with_input(&["amend", "2"], b"a").failed(1),
        "board: amend 2: no such brief\n"
    );
    assert_eq!(world.remote_log(), log);
    a.board(&["next"]).ok();
    let log = world.remote_log();
    assert_eq!(
        a.board(&["block", "1", "Two options:\n## A\nor B"])
            .failed(1),
        "board: block 1: the line `## A` outside a code fence would start a section of its \
         own; use `###` for a heading\n"
    );
    assert_eq!(
        a.board_with_input(&["amend", "1"], b"```\n## quoted\n")
            .failed(1),
        "board: amend 1: a code fence is opened and not closed, which would make the sections \
         after it quoted text\n"
    );
    // Nothing to write is a command line that does not say what to do.
    assert_eq!(
        a.board_with_input(&["amend", "1"], b" \n\n").failed(2),
        "board: amend 1: no text on stdin, which is where it is read from\n"
    );
    a.board(&["report", "1"]).failed(2);
    let said = a.board(&["block", "1", " "]).failed(2);
    assert!(said.contains("the text is empty"), "{said}");
    for args in [&["block", "1"][..], &["block", "c1", "q"], &["amend"]] {
        a.board(args).failed(2);
    }
    assert_eq!(world.remote_log(), log);

    // Reported: no more amendments, and no second report.
    a.board_with_input(&["report", "1"], b"done").ok();
    assert_eq!(
        a.board_with_input(&["amend", "1"], b"late").failed(1),
        "board: amend 1: it is reported, not queued, started or blocked\n"
    );
    assert_eq!(
        a.board_with_input(&["report", "1"], b"again").failed(1),
        "board: report 1: it is reported, not started\n"
    );
    in_step(&world, &a);
}

/// Offline the brief commands commit on the clone, and the next write that
/// reaches the remote pushes them.
#[test]
fn block_amend_and_report_offline_wait_on_the_clone() {
    let world = World::new();
    let a = world.a();
    started(&a, 1, "x");
    a.set_online(false);
    let file = "proj/briefs/1-x.md";
    let ran = a.board(&["block", "1", "q"]);
    assert_eq!((ran.code, ran.stdout.as_slice()), (0, &b"blocked 1\n"[..]));
    assert!(
        ran.stderr.starts_with("board: not pushed: 1 commit ("),
        "{ran:?}"
    );
    let ran = a.board_with_input(&["amend", "1"], b"a");
    assert_eq!(
        (ran.code, ran.stdout.as_slice()),
        (0, &b"amended 1, resumed\n"[..])
    );
    assert!(
        ran.stderr.starts_with("board: not pushed: 2 commits ("),
        "{ran:?}"
    );
    let ran = a.board_with_input(&["report", "1"], b"r");
    assert_eq!((ran.code, ran.stdout.as_slice()), (0, &b"reported 1\n"[..]));
    assert_eq!(header(&world, file, "status").as_deref(), Some("started"));
    assert!(text(a.file(file)).starts_with("id: 1\nstatus: reported\n"));

    a.set_online(true);
    a.board(&["sync"]).ok();
    assert_eq!(header(&world, file, "status").as_deref(), Some("reported"));
    assert_eq!(
        world.remote_log()[..3],
        ["proj: reported 1", "proj: amended 1", "proj: blocked 1"]
    );
    in_step(&world, &a);
}

/// The push of an amendment reached the remote, but `board` heard that it
/// failed: the amendment is not appended a second time.
#[test]
fn an_amendment_that_landed_unheard_is_not_made_twice() {
    let world = World::new();
    let a = world.a();
    started(&a, 1, "x");
    a.board(&["block", "1", "q"]).ok();
    a.hook_once("pre-push", pre_push::LOST_ACK);
    let ran = a.board_with_input(&["amend", "1"], b"once");
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(ran.stdout, b"amended 1, resumed\n");
    assert!(!a.has_hook("pre-push"));
    let file = "proj/briefs/1-x.md";
    assert_eq!(
        after(&world, file, "## Amendments\n"),
        "## Amendments\n\n### <T>\n\nonce\n"
    );
    assert_eq!(header(&world, file, "status").as_deref(), Some("started"));
    in_step(&world, &a);
}

/// The planner's amendment lands while the worker's report pushes: the
/// report is made again on the brief with the amendment, which stays.
#[test]
fn a_report_made_while_an_amendment_lands_keeps_the_amendment() {
    let world = World::new();
    let (a, b) = (world.a(), world.b());
    started(&a, 1, "x");
    b.board(&["sync"]).ok();
    let input = world.dir().join("amendment");
    fs::write(&input, "Mind the cache.\n").unwrap();
    a.hook_once(
        "pre-push",
        &pre_push::window(&b.script_with_input(&["amend", "1"], &input), world.dir()),
    );
    let ran = a.board_with_input(&["report", "1"], b"All done.\n");
    assert_eq!((ran.code, ran.stdout.as_slice()), (0, &b"reported 1\n"[..]));
    assert!(ran.stderr.contains("board: retry 2/8"), "{ran:?}");
    let window = |name: &str| fs::read_to_string(world.dir().join(name)).unwrap();
    assert_eq!(window("window.code"), "0\n");
    assert_eq!(window("window.out"), "amended 1\n");
    assert_eq!(
        after(&world, "proj/briefs/1-x.md", "## Amendments\n"),
        "## Amendments\n\n### <T>\n\nMind the cache.\n## Report\nAll done.\n"
    );
    assert_eq!(
        world.remote_log()[..2],
        ["proj: reported 1", "proj: amended 1"]
    );
    in_step(&world, &a);
}

// ---- file ----

/// A brief that is new, written, queued with `queue_args` after its number,
/// claimed and reported.
fn reported(machine: &Machine, number: u32, slug: &str, queue_args: &[&str]) {
    new_brief(machine, slug);
    write_sections(machine, &format!("proj/briefs/{number}-{slug}.md"));
    let number = number.to_string();
    machine
        .board(&[&["queue", &number][..], queue_args].concat())
        .ok();
    machine.board(&["next"]).ok();
    machine
        .board_with_input(&["report", &number], b"Done.\n")
        .ok();
}

/// A commit in the machine's project repository with this message.
fn commit(machine: &Machine, message: &str) {
    machine.git(
        &machine.proj(),
        ["commit", "-q", "--allow-empty", "-m", message],
    );
}

#[test]
fn file_counts_the_trailer_closes_the_backlog_and_moves_the_brief() {
    let world = World::new();
    let a = world.a();
    for item in ["one", "two", "three"] {
        a.board(&["add", "backlog", item]).ok();
    }
    reported(&a, 1, "x", &["--closes", "b1,b3"]);
    // b3 goes before the brief is filed: skipped.
    a.board(&["drop", "b3"]).ok();
    // Counted: the trailer on a line of its own, trailing blanks allowed.
    commit(&a, "Fix the login\n\nBrief: 1");
    commit(&a, "Fix the logout\n\nCo-Authored-By: x\nBrief: 1  ");
    // Not counted: another brief, or the words inside a line.
    commit(&a, "Other\n\nBrief: 10");
    commit(&a, "Other\n\nAs in Brief: 1");
    commit(&a, "Other\n\nBrief: 1, 2");
    // The user's grep settings do not change what is counted.
    a.git(&a.proj(), ["config", "grep.patternType", "fixed"]);
    let log = world.remote_log();

    assert_eq!(
        a.board(&["file", "1"]).ok(),
        "filed 1 (2 commits) closed b1\n"
    );
    assert_eq!(world.remote_file("proj/briefs/1-x.md"), None);
    let file = "proj/briefs/done/1-x.md";
    assert_eq!(header(&world, file, "status").as_deref(), Some("filed"));
    a_stamp(header(&world, file, "filed"));
    assert!(text(world.remote_file(file)).ends_with("## Report\nDone.\n"));
    assert_eq!(text(world.remote_file("proj/backlog.md")), "- b2 two\n");
    // One commit.
    assert_eq!(world.remote_log()[..2], ["proj: filed 1", &log[0][..]]);
    in_step(&world, &a);
    assert_eq!(a.file("proj/briefs/1-x.md"), None);
    assert_eq!(a.board(&["ls", "briefs"]).ok(), "");
    assert_eq!(
        a.board(&["ls", "briefs", "--status", "filed"]).ok(),
        "1 filed x\n"
    );
    assert!(a.board(&["show", "1"]).ok().contains("status: filed\n"));
}

#[test]
fn file_refuses_what_is_not_a_reported_brief_with_commits() {
    let world = World::new();
    let a = world.a();
    started(&a, 1, "x");
    commit(&a, "Fix\n\nBrief: 1");
    let log = world.remote_log();
    // The status comes before the commits, also outside a repository.
    for at in [a.proj(), a.home()] {
        assert_eq!(
            a.board_in(&at, &["--board", "proj", "file", "1"]).failed(1),
            "board: file 1: it is started, not reported\n"
        );
        assert_eq!(
            a.board_in(&at, &["--board", "proj", "file", "2"]).failed(1),
            "board: file 2: no such brief\n"
        );
    }
    a.board_with_input(&["report", "1"], b"r").ok();
    let log_reported = world.remote_log();
    assert_eq!(log_reported[1..], log[..]);

    // A brief without commits is not filed, unless it says it has none. The
    // commits are counted in the repository the command runs in, also with
    // --board; one without any commit has none with the trailer.
    let other = a.repository("Work/other");
    a.git(&other, ["commit", "-q", "--allow-empty", "-m", "Unrelated"]);
    let empty = a.repository("Work/empty");
    for repository in [&other, &empty] {
        let said = a
            .board_in(repository, &["--board", "proj", "file", "1"])
            .failed(1);
        assert_eq!(
            said,
            format!(
                "board: file 1: no commit reachable from HEAD in {} has the trailer `Brief: 1`; \
                 the worker puts it on every commit of the brief (--no-commits files a brief \
                 that changed nothing there)\n",
                repository.display()
            )
        );
    }
    // Outside a repository there is nothing to count in.
    let said = a
        .board_in(&a.home(), &["--board", "proj", "file", "1"])
        .failed(2);
    assert!(
        said.starts_with(&format!(
            "board: file 1: no repository at {} to count the brief's commits in",
            a.home().display()
        )),
        "{said}"
    );
    for args in [&["file", "b1"][..], &["file"], &["file", "1", "--commits"]] {
        a.board(args).failed(2);
    }
    assert_eq!(world.remote_log(), log_reported);

    assert_eq!(
        a.board_in(&a.home(), &["--board", "proj", "file", "1", "--no-commits"])
            .ok(),
        "filed 1 (no commits)\n"
    );
    assert_eq!(
        a.board(&["file", "1"]).failed(1),
        "board: file 1: it is filed, not reported\n"
    );
    in_step(&world, &a);
}

/// Offline `file` is committed on the clone, and the next write that
/// reaches the remote pushes it.
#[test]
fn file_offline_waits_on_the_clone() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "backlog", "one"]).ok();
    reported(&a, 1, "x", &["--closes", "b1"]);
    commit(&a, "Fix\n\nBrief: 1");
    a.set_online(false);
    let ran = a.board(&["file", "1"]);
    assert_eq!(
        (ran.code, ran.stdout.as_slice()),
        (0, &b"filed 1 (1 commit) closed b1\n"[..])
    );
    assert!(
        ran.stderr.starts_with("board: not pushed: 1 commit ("),
        "{ran:?}"
    );
    assert_eq!(a.file("proj/briefs/1-x.md"), None);
    assert!(a.file("proj/briefs/done/1-x.md").is_some());
    assert!(world.remote_file("proj/briefs/1-x.md").is_some());

    // Meanwhile the other clone adds an item: the two are merged, and the
    // move to done/ is taken from the side that made it.
    world.b().board(&["add", "backlog", "two"]).ok();
    a.set_online(true);
    let ran = a.board(&["sync"]);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert!(
        ran.stderr
            .starts_with("board: merged 1 local commit (by line: proj/backlog.md)"),
        "{ran:?}"
    );
    assert_eq!(world.remote_file("proj/briefs/1-x.md"), None);
    assert!(world.remote_file("proj/briefs/done/1-x.md").is_some());
    assert_eq!(text(world.remote_file("proj/backlog.md")), "- b2 two\n");
    in_step(&world, &a);
}

/// The other clone drops an item the brief closes while `file` pushes:
/// `file` is made again on what the remote has then, and skips the item.
#[test]
fn an_item_dropped_while_file_pushes_is_skipped() {
    let world = World::new();
    let (a, b) = (world.a(), world.b());
    a.board(&["add", "backlog", "one"]).ok();
    a.board(&["add", "backlog", "two"]).ok();
    reported(&a, 1, "x", &["--closes", "b1,b2"]);
    commit(&a, "Fix\n\nBrief: 1");
    b.board(&["sync"]).ok();
    a.hook_once(
        "pre-push",
        &pre_push::window(&b.script(&["drop", "b1"]), world.dir()),
    );
    let ran = a.board(&["file", "1"]);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert!(ran.stderr.contains("board: retry 2/8"), "{ran:?}");
    assert_eq!(ran.stdout, b"filed 1 (1 commit) closed b2\n");
    let window = |name: &str| fs::read_to_string(world.dir().join(name)).unwrap();
    assert_eq!(window("window.code"), "0\n");
    assert_eq!(window("window.out"), "dropped b1\n");
    assert_eq!(text(world.remote_file("proj/backlog.md")), "");
    assert!(world.remote_file("proj/briefs/done/1-x.md").is_some());
    in_step(&world, &a);
}

/// The push of `file` reached the remote, but `board` heard that it failed:
/// it is not made again, which would be refused as filed already.
#[test]
fn a_file_that_landed_unheard_is_not_made_twice() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "backlog", "one"]).ok();
    reported(&a, 1, "x", &["--closes", "b1"]);
    commit(&a, "Fix\n\nBrief: 1");
    a.hook_once("pre-push", pre_push::LOST_ACK);
    let ran = a.board(&["file", "1"]);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(ran.stdout, b"filed 1 (1 commit) closed b1\n");
    assert!(!a.has_hook("pre-push"));
    assert_eq!(
        world
            .remote_log()
            .iter()
            .filter(|subject| *subject == "proj: filed 1")
            .count(),
        1
    );
    in_step(&world, &a);
}
