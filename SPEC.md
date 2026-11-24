# board: spec

A file-based board for a planner/worker pair of coding agents and their user: one small binary, a CLI for the agents and a TUI for the user's terminal pane. Written by a planner session on 2026-11-20 from a design the owner approved; this file is the first brief of the project. Decisions carry their reason so they can be reopened.

This repo is public: it holds the tool only. No project data, client names or ids in code, tests, fixtures or docs.

## Why

Two agent sessions per project: a planner (designs with the user, writes briefs) and a worker (implements one brief at a time, commits). Naming: `board` is the tool (this repo) and one project's view; `board-data` is the repo that holds every project's board. Until now reports travelled as chat messages between the sessions and the open items lived in files only the planner read, per machine. Problems: worker reports cluttered the design conversation; a report died with its transcript; the user could not pull the open items up; two machines had two boards; commit SHAs noted in items went stale after a rebase.

Principle: the files are the record, a message is only a wake-up. The owner works in a terminal multiplexer (herdr) with three panes: work agent, plan agent, board. No Claude plugins: "file based and cli/tui managed".

## Data: the board-data repo

Separate from this repo: a private git repo named `board-data` (GitHub, over https; its owner is configuration, not this repo's business), one clone per machine at `$BOARD_ROOT` (default `~/Work/board-data`). One directory per project:

```
board-data/
  <project>/
    briefs/128-some-slug.md      open briefs
    briefs/done/126-….md         filed briefs
    chores.md
    backlog.md
```

- The project is found from the directory the command runs in: the path of the repository's checkout relative to `~/Work`, `/` replaced by `-` (`~/Work/shop` → `shop`, `~/Work/tools/shop` → `tools-shop`). `--board <name>` or `BOARD` overrides. Reason: derived from existing state, no marker file in the project.
  - A worktree added with `git worktree add` is on the board of the repository it was added to (`~/Work/shop/.claude/worktrees/x` → `shop`), wherever it is. Reason: its own path is one session's choice and mostly a valid project name, so it would give an empty board without a word, and a write there would start and push a project under that name. Where the repository has no checkout of its own to name it (a bare repository, a submodule's worktree), the command is refused and names `--board`. Rejected: refusing every added worktree, which makes a worker in one name the board on every command.
  - The board-data clone itself gives no project: a command run inside `$BOARD_ROOT` without `--board` is refused and names it. Reason: the clone is a repository below `~/Work` too and the place where lists are edited by hand, so its path as a name (`board-data`) would be an empty board, a `lint` that says `ok` right after a hand edit, and a write that starts a project `board-data`. Rejected: taking the project from the directory inside the clone (`board-data/shop` → `shop`), a second way to name a project for one place.
- Three kinds of item:
  - Brief (`128`, a number per project, continuing the existing sequence): a markdown document. Header lines (`id`, `status`, `base`, `closes`, timestamps), then the planner's sections, then `## Questions`, `## Amendments` and `## Report` as they come (a question or an amendment under a `### <time>` line of its own; the report last, as it runs to the end of the file). Status: `draft` → `queued` → `started` ⇄ `blocked` → `reported` → `filed` (moved to `briefs/done/`).
  - Chore (`c12`): the user's own action. A title line under a `## group` heading in `chores.md`, and a body if needed (below): `- c12 <text>. Check: <how to see it is done>  (126)`. A chore without a check is refused, and so is a check with a `Check:` of its own: lint reads after the last one, so only its tail would count. Done = the line is deleted, with its indented lines; git history is the archive.
  - Backlog item (`b41`): a title line in `backlog.md`, and a body if needed (below): `- b41 <text>  (from 126)`. Line order is the user's priority order.
  - A chore or backlog item is a title line and an optional body (owner, 2026-11-24: the model of a brief's header and body, without a file per item). The line is what `ls`, the TUI's rows, the filter and `Y` show, so it is a title that fits a row; a chore's `Check:` and the `(126)` reference are header fields and stay on it, where lint and `add` read them. The body is the indented, non-blank lines directly below the line: `show` and the TUI preview print it with the item, and `done`, `drop`, `move` and the closing of `file` take it along. It is written through `board` (`add … --body`, `amend <c-id|b-id>`, `--replace`) or in the editor; the tool stores each of its lines indented by two spaces (its own indentation kept after them, so a nested list stays one), trailing whitespace trimmed and blank lines left out, as a blank line ends the item for every reader. Rejected: a marker on the row that an item has a body (a second thing to parse off a line agents and `Y` read); one file per item, like briefs (order and groups live in the list file, the ID-keyed merge is built on it, and a line typed by hand would stop working); the check in the body (lint and `add` would have to find it in free text); a blank line inside a body (a second reading of where an item ends, which is where orphaned lines come from).
- The ID is the first word after the dash, so it copies cleanly and greps. A list line without an ID (added by hand in an editor) gets the next ID at the next write that reaches the remote, whichever project it is for.
- Plain markdown, editable in vim. The tool's parser is tolerant of free text after the ID; `board lint` (also run after every write) reports duplicate IDs, a chore without a check, a brief with an unknown status.

## Commands

```
board                          TUI on a terminal; the plain board when piped
board ls [briefs|chores|backlog] [--status <s>]
board show <id>
board new brief <slug>         → path; header written (status draft, base = project HEAD)
board queue <id> [--closes b41,b52]   refuses an empty required section
board next                     worker: claim the brief queued longest (status started) → path; says so when the queue is empty
board block <id> "<question>"  status blocked, question appended under ## Questions
board amend <id> [--replace]   stdin appended under a brief's ## Amendments (a blocked brief resumes), or to an item's body; --replace rewrites the body
board report <id>              stdin becomes ## Report; status reported
board file <id> [--no-commits]  reported brief: verify its commits in the project repo, delete the backlog lines it closes, move to done/
board add chore "<text>" --group <g> --check "<c>" [--from <id>] [--body]
board add backlog "<text>" [--from <id>] [--body]   --body: stdin is the item's body
board done <c-id>…             board drop <b-id>…
board retitle <c-id|b-id> ["<text>"] [--check "<c>"]   the item's line; its ID, place, reference and body stay
board move <c-id|b-id> --before <id> | --after <id>   same list; a chore lands in the group of the item it is put next to
board sync                     board lint
```

- Output is plain, stable text (agents read it); no JSON until something needs it.
- Required brief sections (for `queue`): Task, Design, Premises, Out of scope, Verification, Not doable by the worker, Worker protocol. The template text is the planner skill's, not the tool's: agent rules stay under the owner's review, a data repo or a binary is the wrong home. The skill's draft is this project's to keep true: `skill/my-plan.SKILL.md` (the planner skill as it will read once the CLI exists; every command it names must exist as it names it; a test in `src/main.rs` parses each `board` invocation in its code with the CLI's own definition). It is installed by the owner, who copies it into the chezmoi source (`dot_claude/skills/my-plan/SKILL.md`) after approving it; this project never writes there.
- No roles or auth: which session runs which command is the skill's convention. But `report` and `block` touch only status and their own section, never the planner's text.
- `amend` takes a queued, started or blocked brief: a draft is still the planner's to edit, and a reported brief is done, so what is decided after the report belongs to the next brief; there is no way back from reported (owner, 2026-11-20). A question or an amendment with a `## ` line outside a code fence is refused (use `###`): it would split the brief's section. So is one that opens a fence and leaves it open, which would turn the sections after it into quoted text. A report may hold anything: it is the last section.
- `amend` of a chore or backlog item appends stdin to its body, with no timestamp and no status: an item has neither, and its body is the planner's text to date as it sees fit. `--replace` puts stdin in place of the body, and empty stdin then removes it (owner, 2026-11-24): the planner writes the board only through `board`, and append-only bodies collect text that is no longer true; appending stays the default, as it cannot lose text on the clone. The merge of a sync keeps one side's body whole, the clone's when it changed it, so appends to one item's body on two clones before either synced keep one of them: a body is rarely written from two clones at once, and the other is in the history. `--replace` on a brief is refused: its amendments are a record of what was decided when. `add` reads a body only with `--body`, never "stdin when there is some": an agent's stdin may be a pipe that never closes, and on a terminal the command would wait for the user. Empty stdin is refused, but for `--replace`. `retitle` changes an item's line, below.
- `retitle` replaces an item's text, and a chore's check with `--check` (alone, only the check): the user refers to items by ID and owns the backlog's order, and without it the planner, which writes the board only through `board`, could change a line only by drop and add, a new ID at the bottom. The ID, the place (and group), the trailing reference and the body stay; a part given is written with `add`'s rules (the join before `Check:`, the refusals of a text or check), and a part not given stays byte for byte. It takes a line apart no further than `add` writes one and lint reads it: the reference as `add` writes it (`(126)` on a chore, `(from 126)` on a backlog item), a chore's check after its last `Check:`; the rest is text, and a new check alone leaves the line before it byte for byte. A chore with no check to keep needs `--check`. Not for a brief: its title is in its file, its slug in the file name. A line retitled on two machines before a sync takes the clone's, as two hand edits of a line do. Rejected: `edit` as the name (it reads like the TUI's `e`, the editor); `--group` (`move` owns the group); `amend --title` (amend writes the body, and `--replace` would become ambiguous).
- `file` verifies commits by the trailer `Brief: <id>` on a line of its own, in the repository the command runs in (also with `--board`), reachable from HEAD: the worker puts it on every commit of a brief. Decided by the owner, 2026-11-20, over SHAs noted in the report: a rebase changes every SHA and keeps the message. A brief without any such commit is not filed (`--no-commits` for a brief that changed nothing in the repo). Filing is one commit: the backlog items `closes:` names are deleted with their indented lines, the brief is stamped filed and moved to `briefs/done/`; it prints `filed 128 (3 commits) closed b41`. An item already gone (dropped since the brief was queued) is skipped, as the item is closed either way; one on two lines is refused, as deleting both would guess.

## Sync

The tool is the only thing that runs git in the board-data repo. It pushes: the board-data repo is data, the owner's "never push" rule for code does not apply to it (confirmed 2026-11-20: "a totally different scope, intention and ruleset").

- Every write: take a lock (two sessions on one machine share one clone), commit hand edits to board files (`board: hand edits`), fetch, merge, apply the change on the merged state, commit only the touched paths (one commit per command, subject `<project>: <what>`), push, then fast-forward the checkout. Every write commits hand edits, not only `sync`: the change applies to the merged commit, so an edit left in the worktree would take no part. A file saved while the write runs is never overwritten: the fast-forward refuses, and the save is committed and merged in.
- Local commits (hand edits, offline writes) join the remote's by a merge commit (`board: merge`), never by a rebase: ancestry then records what is already pushed, so a crash between push and local update cannot apply a hand edit twice. Cost: merge commits in history when both sides changed.
- A failed push: unless one of the command's pushes is found to have landed (its commit is an ancestor of the fetched remote; a push can reach the remote and still report failure, even after the next fetch, and re-applying then would publish the item twice), the command's commit is dropped and the command applied again on the fresh remote state, at most 4 pushes; an ID is printed only after the push that publishes it, so nothing is ever renumbered. A command's commit carries a mark of its run (`Board-Run:`): two clones of one user making the same change on the same commit in the same second would otherwise make the same commit, and each would take the other's push for its own (two workers handed one brief by `next`). Four failed pushes count as offline, even with a working fetch: an agent's command must end rather than retry without limit, and an ID not yet published may not be printed.
- Offline: a write that allocates an ID is refused, and so is `next` (a claim is shared like an ID: two offline claims of one brief would conflict); other writes commit locally and are pushed by the next command. Accepted by the owner, 2026-11-20: "we need to be online for the agents anyway".
- The next list ID and the next brief number are one past the highest ever used in git history (the list file's lines, the files ever added under the project's `briefs/`), not only among what is there now: a deleted line or a hand-deleted brief would otherwise free its number, and `file` would count the old brief's `Brief:` commits. History is the counter, so a shallow clone is refused. Rejected: a counter file, which a hand edit makes stale.
- `chores.md` and `backlog.md` are merged by the tool itself, keyed by ID: adds from two machines both survive, a deleted ID stays deleted, a moved line is not duplicated, an item's indented lines stay under it and go with it, also when the other machine edited them meanwhile (the delete wins, as for the item's line). That holds for an item with an ID: below one typed by hand without one yet, they are counted as text, as the item is, until the next write gives it its ID. Hand-typed duplicate IDs keep both lines (lint reports them): renumbering one would guess which line `closes b41` meant. No `merge=union` and no `.gitattributes`: a plain union brings a deleted line back when the other machine edited or moved it.
- A brief changed on two machines is never merged as text: the remote version stays, the local one is saved beside it as an untracked `<brief>.local`, and `lint` reports that copy after every write in its project until it is deleted. Rejected: stopping the sync, which blocks every project on that machine with git by hand as the only way out. See Open points.
- Reads never touch the network. The TUI fetches in the background about once a minute and shows unpushed commits, commits the remote is ahead and the last fetch in its status line; the fetch takes the write lock and is skipped when a command holds it, so every git process in the clone still runs under the lock. It only fetches: committing and pushing on a timer would publish a half-finished hand edit, and the owner wants commits on meaningful transitions only (2026-11-21).
- Shell out to the `git` binary (the machines' credential helper already handles https); no libgit2.

## TUI

`board` on a terminal, laid out like LazyGit (owner, 2026-11-21): panels inside one terminal pane, built for a wide one (about 200 columns; no narrow layout, nobody uses one).

```
╭─[1]─Briefs - Filed──────────────────╮╭─[0]─128 some-slug─────────────────────────╮
│▶128 started   some-slug        42m  ││id: 128                                    │
│ 127 reported  other-slug            ││status: started                            │
╰─────────────────────────────────────╯│                                           │
╭─[2]─Chores──────────────────────────╮│# Title                                    │
│ ## deploy                           ││## Task                                    │
│ c12 Rotate the key. Check: …  (126) ││…                                          │
╰─────────────────────────────────────╯│                                           │
╭─[3]─Backlog─────────────────────────╮│## Report                                  │
│ b41 …                    (from 126) ││                                           │
╰─────────────────────────────────────╯╰───────────────────────────────────────────╯
 shop · fetched 1m ago · 2 unpushed · 128 started 42m         d done  e edit  ? help
```

- Left, about 45% of the width and at most 90 columns: three stacked panels, each with its own cursor; the focused one takes the spare height. Briefs (tabs Briefs, the open ones, and Filed; open ones started and blocked first, then reported, queued, draft), chores by group, backlog in order. Titles as LazyGit draws them (owner, 2026-11-23): the jump key in brackets, then the tabs separated by ` - `, the panel's name being its first tab; the tab shown is bold, green when the panel has the focus. Rounded corners. The preview is `[0]`, as LazyGit numbers its main view, with the same green; its number shows with nothing to preview too, as it names the pane. The number is a key, `0`, as on the panels: a number without its key would promise a jump that is not there. A line too long for its panel is cut there and whole in the preview.
- Colour: an item's ID is cyan, in its panel and in the preview's title; an open brief's status is coloured by what it asks of the user (`blocked` red, `started` green, `reported` yellow, `draft` dim, `queued` plain); the age of a brief in flight is dim. A row pending a move is yellow as a whole. Only the terminal's 16 named colours, never RGB or indexed: the user's terminal theme decides what they look like, so the board reads on a light and a dark theme, and there is no second theme to maintain. Headings, references, the preview's body and the brief in flight on the status line stay plain: the ID and the status are what the eye looks up.
- Right: the preview follows the cursor: a brief with its report, or the wrapped line of a chore or backlog item. Plain text, headings highlighted; no markdown renderer, which is much machinery for a pane briefs are read in.
- Bottom line: project, sync state, the brief in flight and for how long, then key hints. Confirmations, sync notes and errors show here too.
- The TUI edits no text: `e` suspends it and runs `$EDITOR` on the brief, or on the list file at the item's line (`+N`), and syncs on return (which commits the edit). It waits for the TUI's own writes to end first: an editor opened beside one reads the file before the write changes it, and saving would put back what the write took out. Any key but Esc calls the wait off, so the editor never takes the terminal while keys meant for the TUI are being typed. Its only writes are whole-item actions.
- Keys, vim-flavoured: `j`/`k`, `gg`/`G`, `ctrl-d`/`ctrl-u`, `/` filter (the focused panel), `1` `2` `3` or `h`/`l`/Tab/Shift-Tab focus a panel, `[`/`]` switch the Briefs tab, `0` or Enter focuses the preview for scrolling (with an item under the cursor), `y` yanks the ID, `Y` the line (as `board ls` prints it), `d` done (chore), `x` drop (backlog), `J`/`K` move an item, `e` editor, `r` sync, `?` help, `q` quit (ctrl-c too, from anywhere: raw mode makes it a key). `d` and `x` ask once, on the bottom line: `y` is yes, any other key no. Briefs have no actions: queue and file are the planner's. Quit waits for the TUI's writes to end, so what the screen showed done is on the board; a second quit does not wait (a push can stall as long as the connection does).
- Esc leaves the preview, closes help, answers no to a confirmation or clears a filter, and does nothing else: the owner hits Esc by reflex. A run of `J`/`K` stays pending over it.
- Every action is a CLI command, so an agent can do what the user does, and the TUI calls the same function the command does (in process, not a second `board`). `J`/`K` is `board move`: the row moves on screen at once and is marked pending, and one `move` is sent when the moving ends (the first key that is not `J`/`K` or Esc; `r` and quit send it before their own command), so a run of presses is one commit and no press waits for a push. Rejected: a commit per press (a push each, unusable) and an idle timer (a commit on no transition). A pending move whose item or neighbour is gone after a reload is dropped with a note, as is one whose place no command can name (no neighbour with an ID, an ID on two lines).
- A command runs on a worker thread, one at a time, so navigation never waits for the network. Results and the commands' notes show on the bottom line.
- No mouse capture, so the terminal's own selection and copy keep working. Yank through OSC 52, and through `wl-copy` (under Wayland) / `pbcopy` (macOS) when present.
- Redraw when a file under the board changes (the worker's `next` and `report` show at once); the cursor stays on its item, not its row.
- Tests: the state is a pure function of events (key, resize, clock tick, file change, command result) giving the new state and the commands to run; a tick carries the time, and the fetch timer compares it with the time of the last fetch asked for, so a trace drives the timer by the times it sends, without waiting. Trace-test it with random event sequences; after every step: the cursor is on a row that exists and on the same item across a reload, Esc never starts a command and navigation none of its own (it sends the move of the `J`/`K` run it ends), `d`/`x` never run unconfirmed, a pending move is sent exactly once or dropped with a note. Rendering is tested against ratatui's in-memory screen; the runtime (worker, editor, yank) against the real sync loop, on a screen in memory.

## Consolidating the old planner folders: not this tool's job

No `import` command. Each project's old planner folders (one per machine: `backlog.md`, `chores.md`, `queue/`, `sent/`, `done/`) are moved onto the board by that project's own agents, who know what the items mean and can merge two machines' lists with judgement (the owner, 2026-11-20: "the agent only needs CRUD tools from the cli for it to do so"). What that asks of the tool, all of it ordinary use:

- `board add chore|backlog` for every open line (IDs are assigned fresh; nothing refers to old ones).
- Filed briefs are an archive: the agent copies the old files into `briefs/done/` and runs `board sync`, which commits hand-placed files as it commits hand edits. `lint` accepts an archived brief without the header the tool writes (number and slug come from the file name).
- The next brief number is one past the highest number among open and filed briefs, those deleted since included (Sync), so a project's sequence continues.

## Quality bar

The owner's rules (global CLAUDE.md) apply: guarantees over prose, commit when green, fresh-eyes review.

- Tests run the real thing: temp dirs, a bare repo as the remote, two clones.
- Trace-test the sync: random interleavings of the writing commands on two clones (with pushes failing and going offline in between); after every step: no item lost, no duplicate ID, both clones converge after a sync. Prove the check is not vacuous by breaking the lock or the retry once.
- No sleeps in tests.
- Parser round-trip: reading and writing a list file changes nothing but what the command changed (hand edits survive).

## Build order

1. CLI (everything under Commands and Sync). The planner skill switches to it when this is done; each project's agents then consolidate their old folders onto the board.
2. TUI, in three commits: `board move`; the read-only TUI (panels, preview, navigation, filter, tabs, help, file watcher, background fetch and status line); the actions (`d`, `x`, `J`/`K`, `e`, `r`, yank).
3. Later, only if asked: chore checks the tool can run itself (a command per chore, run on demand, never automatically); herdr pane metadata.

Stack: Rust, `clap`, `ratatui` + `crossterm`, a file watcher (`notify`), property tests (`proptest`). Install with `cargo install --path .` on each machine.

## Not in scope

- A Claude plugin, a web UI, a daemon or a systemd/launchd unit.
- Sending messages between sessions: the skill does the two wake-ups (planner → worker "queue has work", worker → planner "blocked").
- Notifications: herdr already shows the worker's state.

## Open points

- A brief changed on two machines: copy-and-continue (Sync) is chosen while the board has one user, the owner, who consolidates `.local` copies by hand. Reconsider when it has more; the alternative is stopping the sync and printing the git commands that resolve it.
