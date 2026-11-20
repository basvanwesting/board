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

#[test]
fn four_refused_pushes_change_nothing_and_exit_3() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "chore", "x", "--group", "g", "--check", "x"])
        .ok();
    let before = world.remote_main();
    world.reject_pushes(9);
    for (args, what) in [
        (&["add", "backlog", "x"][..], "add backlog"),
        (&["done", "c1"], "done"),
    ] {
        let said = a.board(args).failed(3);
        let start = format!(
            "board: retry 2/4\nboard: retry 3/4\nboard: retry 4/4\n\
             board: {what}: not pushed after 4 attempts (! [remote rejected] "
        );
        assert!(said.starts_with(&start), "{said}");
        assert!(
            said.ends_with(" -> main (pre-receive hook declined)); nothing was changed\n"),
            "{said}"
        );
    }
    assert_eq!(world.remote_main(), before);
    in_step(&world, &a);
    assert_eq!(a.file("proj/backlog.md"), None);
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

#[test]
fn without_the_remote_a_write_changes_nothing_and_exits_3() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "chore", "x", "--group", "g", "--check", "x"])
        .ok();
    let gone = world.dir().join("gone");
    a.git(
        &a.data(),
        ["remote", "set-url", "origin", gone.to_str().unwrap()],
    );
    let main = a.main();
    for (args, what) in [
        (&["add", "backlog", "x"][..], "add backlog"),
        (&["done", "c1"], "done"),
        (&["sync"], "sync"),
    ] {
        let said = a.board(args).failed(3);
        assert!(
            said.starts_with(&format!(
                "board: {what}: needs the remote, which was not reached (fatal: "
            )),
            "{said}"
        );
    }
    assert_eq!(a.main(), main);
    assert_eq!(a.status(), "");
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

/// git's lock on the tool's index, left by a command killed while git built
/// its commit: the next write goes on.
#[test]
fn a_stale_lock_on_the_tools_index_is_removed() {
    let world = World::new();
    let a = world.a();
    let lock = a.data().join(".git").join("board.index.lock");
    fs::write(&lock, "").unwrap();
    assert_eq!(a.board(&["add", "backlog", "x"]).ok(), "b1\n");
    assert!(!lock.exists());
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
    // Makes a signature git takes, to sign a commit by hand.
    let signer = script(
        "signer",
        "echo '[GNUPG:] SIG_CREATED ' >&2\n\
         printf -- '-----BEGIN PGP SIGNATURE-----\\n\\nfake\\n-----END PGP SIGNATURE-----\\n'\n",
    );
    // Signs nothing, and says an id line when asked to check a signature.
    let gpg = script("gpg", "echo '+- b99 not a line' >&2\nexit 1\n");
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
    // An untracked board file is still seen when untracked files are hidden.
    a.write("shop/chores.md", "- c1 hand. Check: x\n");
    assert_eq!(a.board(&["add", "backlog", "four"]).code, 4);
}

/// A board file changed by hand, or a clone ahead of and behind the remote
/// at once: a write cannot take either into account yet, so it refuses
/// rather than lose the change.
#[test]
fn a_clone_with_changed_board_files_or_diverged_from_the_remote_is_refused() {
    let world = World::new();
    let (a, b) = (world.a(), world.b());
    a.board(&["add", "backlog", "one"]).ok();
    a.write("proj/backlog.md", "- b1 edited by hand\n");
    let said = a.board(&["add", "backlog", "two"]).failed(4);
    assert_eq!(
        said,
        format!(
            "board: board files changed by hand in {} (proj/backlog.md); a write does not \
             commit them yet: commit them with git on top of the remote's main first \
             (git stash -u; git pull --ff-only; git stash pop; git commit), or restore them\n",
            a.data().display()
        )
    );
    // A file that is not the board's is not in the way.
    a.git(&a.data(), ["checkout", "--", "proj/backlog.md"]);
    a.write("README.md", "notes\n");
    a.write("proj/briefs/128-x.md.local", "a copy\n");
    assert_eq!(a.board(&["add", "backlog", "two"]).ok(), "b2\n");
    fs::remove_file(a.data().join("README.md")).unwrap();
    fs::remove_file(a.data().join("proj/briefs/128-x.md.local")).unwrap();

    // Diverged: a commit by hand here, another write there.
    b.board(&["add", "backlog", "from b"]).ok();
    a.write("shop/backlog.md", "- b1 by hand\n");
    a.git(&a.data(), ["add", "-A"]);
    a.git(&a.data(), ["commit", "-q", "-m", "by hand"]);
    let said = a.board(&["add", "backlog", "three"]).failed(4);
    assert_eq!(
        said,
        format!(
            "board: main of {} and the remote's main have both moved on; a write does not \
             merge them yet\n",
            a.data().display()
        )
    );
}

/// A file saved while the write's push is on its way: the change is on the
/// remote, the clone's `main` and files stay behind, and the save is kept.
/// The next write refuses the changed file, and the way out its message
/// names gets the clone writing again with the save pushed.
#[test]
fn a_file_saved_during_the_push_is_kept_and_the_refusal_names_the_way_out() {
    let world = World::new();
    let a = world.a();
    a.board(&["add", "backlog", "one"]).ok();
    a.board(&["add", "backlog", "two"]).ok();
    let before = a.main();
    a.hook_once(
        "pre-push",
        "printf '%s\\n' '- b1 one, edited' '- b2 two' >proj/backlog.md\n",
    );
    let ran = a.board(&["add", "backlog", "three"]);
    assert_eq!((ran.code, ran.stdout.as_slice()), (0, &b"b3\n"[..]));
    assert!(
        ran.stderr.starts_with("board: local files not updated: "),
        "{ran:?}"
    );
    assert_eq!(
        text(world.remote_file("proj/backlog.md")),
        "- b1 one\n- b2 two\n- b3 three\n"
    );
    assert_eq!(a.main(), before);
    assert_eq!(
        text(a.file("proj/backlog.md")),
        "- b1 one, edited\n- b2 two\n"
    );

    assert_eq!(a.board(&["add", "backlog", "four"]).code, 4);
    let data = a.data();
    a.git(&data, ["stash", "-q", "-u"]);
    a.git(&data, ["pull", "-q", "--ff-only"]);
    a.git(&data, ["stash", "pop", "-q"]);
    a.git(&data, ["commit", "-q", "-am", "edit"]);
    assert_eq!(a.board(&["add", "backlog", "four"]).ok(), "b4\n");
    assert_eq!(
        text(world.remote_file("proj/backlog.md")),
        "- b1 one, edited\n- b2 two\n- b3 three\n- b4 four\n"
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
