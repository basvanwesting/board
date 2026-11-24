# board

A file-based board for a pair of coding agents and their user: a planner session that designs work with the user and writes it down as briefs, a worker session that takes one brief at a time, builds it and commits, and the user who watches it all from a terminal pane. One small binary: a CLI for the agents, a TUI for the user.

It exists because the alternative, agents sending each other reports as chat messages, loses things: a report lands in the middle of a design conversation and dies with its transcript, the open items live in files only one agent reads, and each machine ends up with a board of its own. Here the files are the record, kept in one git repository that every machine shares; a message between the sessions is only a wake-up.

[SPEC.md](SPEC.md) is the full spec, with the reason behind each decision. This file says what the tool does and how to use it.

## The model

Three parties:

- **Planner**: an agent session that designs with the user and writes briefs. It writes nothing but the board.
- **Worker**: an agent session in the same project that claims the next brief, implements it, commits and reports.
- **User**: approves designs, does what only a human can, and keeps the board open in a pane of their own.

Three lists, one board per project:

- **Briefs** (`126`): one markdown file per piece of work: the planner's design, then the worker's questions and report. Numbered per project. Written by the planner, moved along by both; it ends filed.
- **Chores** (`c12`): an action of the user's that a brief leaves behind (a deploy, an edit on production, something to judge by eye). A title line under a group heading, with a check that tells it is done, and a body below it if needed: a chore expires unannounced, and the check is how to see it did. It ends deleted, by the user or the planner.
- **Backlog** (`b41`): an unrefined item for later: a title line, and a body below it if needed. Line order is the user's priority. Added on the user's word or from a worker's out-of-scope findings; it ends deleted, closed by the brief that resolves it or dropped.

The ID is the first word after a list line's dash (`- b41 …`), so it copies and greps cleanly. What does not fit an item's line goes in its body, the indented lines below it: written from stdin with `board add backlog "<text>" --body`, added to with `board amend <id>` and rewritten with `board amend <id> --replace`, or edited by hand; `board show <id>` and the TUI's preview show it, and it goes when the item does. A chore's check stays on its line. Git history is the archive of everything deleted.

## The life of a brief

```text
draft → queued → started ⇄ blocked → reported → filed
```

| Status | Meaning | Who moves it there | Command |
|---|---|---|---|
| `draft` | The planner is writing it; the header is in place. | planner | `board new brief <slug>` |
| `queued` | Approved and complete, waiting for the worker. Refused while a required section is empty. | planner | `board queue <id>` |
| `started` | The worker claimed it, the one queued longest. Also where a blocked brief goes back to. | worker, then planner | `board next`, `board amend <id>` |
| `blocked` | The worker stopped on a question, written into the brief. | worker | `board block <id> "<question>"` |
| `reported` | Done: the worker's report is the last section. | worker | `board report <id>` |
| `filed` | Checked and closed; the file moves to `briefs/done/`. | planner | `board file <id>` |

`board queue <id> --closes b41,b52` names the backlog items the brief resolves. `board file <id>` checks the repository it runs in, also when the board is named (`board file <id> --board shop`), for commits carrying the trailer `Brief: <id>` on a line of their own (a rebase keeps it, where it would change every SHA), refuses a brief with none (`board file <id> --no-commits` for one that changed no code, such as an investigation), deletes the backlog items it closes and moves the brief to `briefs/done/`. `board amend <id>` adds to a queued, started or blocked brief without changing who has it; only on a blocked one does it resume the work.

## Install and setup

```sh
cargo install --path .
```

The boards live in a second git repository, `board-data`: private, with a remote every machine reaches over https, cloned once per machine at `$BOARD_ROOT` (default `~/Work/board-data`). The clone must be a full one, not shallow (`git clone --depth`): IDs are counted from its history. It must be on branch `main`, with its remote named `origin` and at least one commit; the tool creates everything inside it.

A command finds its project's board from the directory it runs in: the path of the git checkout below `~/Work`, with `/` turned into `-` (`~/Work/shop` is `shop`, `~/Work/tools/shop` is `tools-shop`). Elsewhere, name the board yourself: `board ls --board shop`, or `BOARD=shop` in the environment. A project has no setup of its own: its first write creates its directory on the board.

```text
board-data/
  shop/
    briefs/1-fix-login.md
    briefs/done/
    chores.md
    backlog.md
```

## Usage

The planner runs `board new brief <slug>`, `board queue <id>`, `board amend <id>`, `board file <id>`, `board add chore "<text>" --group <group> --check "<check>" --from <id>`, `board add backlog "<text>" --from <id>`, `board retitle <id> "<text>"` (an item's line, keeping its ID and place) and `board done <id>`, and reads with `board ls`, `board ls briefs --status reported` and `board show <id>`. The worker runs `board next`, `board block <id> "<question>"` and `board report <id>`. `board --help` is the reference, and each command has its own (`board queue --help`).

A session in `~/Work/shop`, from a backlog item to a filed brief:

```sh
$ board add backlog "Login fails on a long password"
b1
$ board new brief fix-login
~/Work/board-data/shop/briefs/1-fix-login.md
# the planner writes the brief's sections into that file
$ board queue 1 --closes b1
queued 1
$ board next
~/Work/board-data/shop/briefs/1-fix-login.md
$ board block 1 "Which hash does the login use?"
blocked 1
$ echo "bcrypt, as everywhere else" | board amend 1
amended 1, resumed
# the worker commits with the trailer "Brief: 1"
$ echo "All green; deploy not done." | board report 1
reported 1
$ board add chore "Deploy the login fix" --group deploy --check "log in with a long password on production" --from 1
c1
$ board file 1
filed 1 (1 commit) closed b1
$ board done c1
done c1
```

The user runs `board` without a command for the TUI (piped, it prints the board as `board ls` does). On the left three panels: Briefs (tabs Briefs, the open ones, and Filed), Chores by group, Backlog in order, each titled with its jump key (`[1]`); on the right a preview of the item under the cursor, `[0]`; at the bottom the project, the sync state and the brief in flight. The keys to start with:

- `j`/`k` move, `1` `2` `3` or Tab focus a panel, `0` or Enter scrolls the preview, `/` filters the panel.
- `d` marks a chore done, `x` drops a backlog item (each asks once), `J`/`K` reorder, `e` opens the item in `$EDITOR`.
- `r` syncs, `?` shows every key, `q` quits. Esc only ever backs out.

The TUI edits no text itself: every action is the CLI command of the same name, so an agent can do whatever the user does.

## Sync

The tool is the only thing that runs git in the board-data clone, and it pushes: the board is shared data, not code waiting for review. Every write takes a lock, commits any hand edits, fetches and merges, applies the change, commits and pushes; reads never touch the network. Editing the list files or a brief in an editor is fine: the next write commits it. `board sync` brings the clone and the remote together without a change of its own; `board lint` checks the files (duplicate IDs, a chore without a check, an unknown status) and is also run after every write. A write that hands out an ID needs the network, and so does `board next`; other writes commit locally and go out with the next push.

## The planner skill

[`skill/my-plan.SKILL.md`](skill/my-plan.SKILL.md) is the planner's instructions that go with the tool: the loop, how to talk to the user, and the brief template with the worker's protocol. It is written for Claude Code; install it by copying it into your own agent setup (for example `~/.claude/skills/my-plan/SKILL.md`). A test parses every `board` command it names, as it does those in this file.

## Development

Green is:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

MIT licensed.
