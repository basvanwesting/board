//! `--local` through the binary: a write that commits on the clone and
//! leaves the network alone, published by a later `board sync`, and the
//! commands it is refused on.

mod common;

use std::io::Read;
use std::process::Stdio;
use std::time::{Duration, Instant};

use common::{Machine, World};

/// What the planner writes below a brief's header: text in every required
/// section.
const SECTIONS: &str = "# Fix the login\n\n## Task\nt\n## Design\nd\n## Premises\np\n\
                        ## Out of scope\no\n## Verification\nv\n\
                        ## Not doable by the worker\nn\n## Worker protocol\nw\n";

/// A brief that is new, written and queued.
fn queued(machine: &Machine, number: u32, slug: &str) {
    machine.board(&["new", "brief", slug]).ok();
    let file = format!("proj/briefs/{number}-{slug}.md");
    let mut bytes = machine.file(&file).expect("the brief is there");
    bytes.extend_from_slice(SECTIONS.as_bytes());
    machine.write(&file, bytes);
    machine.board(&["queue", &number.to_string()]).ok();
}

/// A chore `c1` and a started brief `1` on `a`, and a commit on the
/// remote, made on `b`, that `a` has not fetched.
fn a_board_behind(world: &World) {
    let (a, b) = (world.a(), world.b());
    a.board(&["add", "chore", "x", "--group", "g"]).ok();
    queued(&a, 1, "s");
    a.board(&["next"]).ok();
    assert_eq!(b.board(&["add", "backlog", "from b"]).ok(), "b1\n");
    assert_ne!(a.origin_main(), world.remote_main());
}

/// Where the remote and the clone's view of it are, to compare after a run
/// that must have touched neither.
fn remote_and_fetched(world: &World, machine: &Machine) -> (String, String) {
    (world.remote_main(), machine.origin_main())
}

#[test]
fn a_flagged_write_commits_on_the_clone_and_sync_publishes_it() {
    let world = World::new();
    let a = world.a();
    a_board_behind(&world);
    let before = remote_and_fetched(&world, &a);

    // `ok` asserts an empty stderr: no `not pushed` note of either form,
    // no `retry`.
    assert_eq!(
        a.board(&["retitle", "c1", "y", "--local"]).ok(),
        "retitled c1\n"
    );
    assert_eq!(remote_and_fetched(&world, &a), before);
    let ran = a.board_with_input(&["amend", "1", "--local"], b"More.\n");
    assert_eq!(
        (ran.code, ran.stdout.as_slice(), ran.stderr.as_str()),
        (0, &b"amended 1\n"[..], "")
    );
    assert_eq!(remote_and_fetched(&world, &a), before);
    assert_eq!(a.log()[..2], ["proj: amended 1", "proj: retitle c1"]);
    assert_eq!(a.status(), "");

    // The sync pushes both, merged with what `b` pushed meanwhile.
    let ran = a.board(&["sync"]);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(ran.stdout, b"synced\n");
    assert_eq!(a.main(), world.remote_main());
    let log = world.remote_log();
    assert!(log.contains(&"proj: amended 1".to_owned()), "{log:?}");
    assert!(log.contains(&"proj: retitle c1".to_owned()), "{log:?}");
    assert!(a.file("proj/backlog/b1.md").is_some(), "b's item came in");
}

#[test]
fn a_flagged_write_with_the_remote_gone_says_nothing_about_it() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "chore", "x", "--group", "g"]).ok();
    a.set_online(false);
    assert_eq!(
        a.board(&["retitle", "c1", "y", "--local"]).ok(),
        "retitled c1\n"
    );
    assert_eq!(a.log()[0], "proj: retitle c1");
}

/// The sweep as the planner skill writes it: a sync, a flagged command per
/// item, a sync. The three commits arrive in one push.
#[test]
fn a_sweep_of_flagged_writes_goes_out_in_one_push() {
    let world = World::new();
    let (a, b) = (world.a(), world.b());
    for text in ["one", "two", "three"] {
        b.board(&["add", "backlog", text]).ok();
    }
    a.board(&["sync"]).ok();
    let before = world.remote_main();
    for (id, text) in [("b1", "uno"), ("b2", "dos"), ("b3", "tres")] {
        assert_eq!(
            a.board(&["retitle", id, text, "--local"]).ok(),
            format!("retitled {id}\n")
        );
        assert_eq!(world.remote_main(), before, "{id}: the remote did not move");
    }
    a.board(&["sync"]).ok();
    assert_eq!(a.main(), world.remote_main());
    assert_eq!(
        world.remote_log()[..4],
        [
            "proj: retitle b3",
            "proj: retitle b2",
            "proj: retitle b1",
            "proj: add b3"
        ]
    );
}

/// Every command the flag does not fit: a usage error before anything is
/// read, fetched or committed.
#[test]
fn the_flag_is_refused_where_it_does_not_fit() {
    let world = World::new();
    let a = world.a();
    a_board_behind(&world);
    let before = (world.remote_main(), a.origin_main(), a.main());
    let unchanged = |what: &str| {
        assert_eq!(
            (world.remote_main(), a.origin_main(), a.main()),
            before,
            "{what}"
        );
        assert_eq!(a.status(), "", "{what}");
    };
    for (args, what) in [
        (&["add", "chore", "x", "--local"][..], "add chore"),
        (&["add", "backlog", "x", "--local"][..], "add backlog"),
        (&["new", "brief", "t", "--local"][..], "new brief"),
        (&["sync", "--local"][..], "sync"),
        (&["ls", "--local"][..], "ls"),
        (&["show", "c1", "--local"][..], "show"),
        (&["lint", "--local"][..], "lint"),
        (&["--local"][..], "board"),
    ] {
        let stderr = a.board(args).failed(2);
        assert!(
            stderr.starts_with(&format!("board: {what} --local: ")),
            "{stderr}"
        );
        unchanged(what);
    }
    let stderr = a.board(&["add", "chore", "x", "--local"]).failed(2);
    assert!(stderr.contains("needs the remote"), "{stderr}");
    let stderr = a.board(&["sync", "--local"]).failed(2);
    assert!(stderr.contains("publishes"), "{stderr}");

    // `next` with a brief queued: it stays queued.
    queued(&world.b(), 2, "t");
    a.board(&["sync"]).ok();
    let before = (world.remote_main(), a.origin_main(), a.main());
    let unchanged = |what: &str| {
        assert_eq!(
            (world.remote_main(), a.origin_main(), a.main()),
            before,
            "{what}"
        );
    };
    assert!(
        a.board(&["next", "--local"])
            .failed(2)
            .starts_with("board: next --local: ")
    );
    unchanged("next");
    let brief = a.file("proj/briefs/2-t.md").expect("brief 2");
    assert!(String::from_utf8(brief).unwrap().contains("status: queued"));

    // In the clone itself, where a bare sync runs.
    assert!(
        a.board_in(&a.data(), &["sync", "--local"])
            .failed(2)
            .starts_with("board: sync --local: ")
    );
    unchanged("sync in the clone");

    // stdin is not read: the refusal comes with stdin left open.
    let mut child = a
        .command()
        .args(["add", "chore", "x", "--body", "--local"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run board");
    let _open = child.stdin.take();
    // A bounded poll: nothing announces that a process did not read.
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait for board") {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().expect("kill board");
            panic!("add chore --body --local read stdin");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("piped")
        .read_to_string(&mut stderr)
        .unwrap();
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(stderr.starts_with("board: add chore --local: "), "{stderr}");
    unchanged("add chore --body");
}

/// Guard: without the flag the same commands fetch and push.
#[test]
fn without_the_flag_the_same_writes_push() {
    let world = World::new();
    let a = world.a();
    a_board_behind(&world);
    let ran = a.board(&["retitle", "c1", "y"]);
    assert_eq!(
        (ran.code, ran.stdout.as_slice()),
        (0, &b"retitled c1\n"[..])
    );
    assert_eq!(a.main(), world.remote_main());
    assert_eq!(world.remote_log()[0], "proj: retitle c1");
    let ran = a.board_with_input(&["amend", "1"], b"More.\n");
    assert_eq!((ran.code, ran.stdout.as_slice()), (0, &b"amended 1\n"[..]));
    assert_eq!(a.main(), world.remote_main());
    assert_eq!(world.remote_log()[0], "proj: amended 1");
}
