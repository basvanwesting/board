//! The brief commands through the binary: `new brief`, `queue`, `next`, on
//! two clones of one bare remote.

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
    assert!(ran.stderr.contains("board: retry 2/4"), "{ran:?}");
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
