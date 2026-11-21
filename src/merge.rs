//! The merge of a clone's own commits (hand edits, writes made offline) with
//! the remote's: which file of which side stands, and the lines of a list
//! file merged by id. Pure, so every rule is tested without git; `sync.rs`
//! reads the trees, runs `git merge-file --union` for a list's line order and
//! writes what this decides.
//!
//! One three-way merge per write, of the clone's `main` and the remote's,
//! keyed by item rather than by path or by commit:
//!
//! - Intermediate local commits are never replayed one by one, so they
//!   cannot conflict with each other. Rejected: `git rebase` with a merge
//!   driver, which rewrites the local commits (after a kill the clone could no
//!   longer tell what it had published) and needs rebase recovery.
//! - A brief is one key under `briefs/` and `briefs/done/`: a brief filed on
//!   one side is the same brief the other side edited. Rejected: git's rename
//!   detection, whose modify/delete and rename cases would have to be
//!   untangled again here.
//! - A list file is merged by line, and its lines by id: a delete beats an
//!   edit and a move, and an item's indented lines stay under it, counted as
//!   text: a delete of the item does not undo an edit of them. Rejected:
//!   `merge=union`, which brings a deleted line back when the other side
//!   edited or moved it.

use std::collections::{BTreeMap, BTreeSet};

use crate::id::{BriefId, ListKind};
use crate::list::{Entry, is_continuation, parse_line, with_id};
use crate::project::{BoardPath, ProjectName};
use crate::raw;

/// A tree as path to entry; `B` is what git has for a path (its mode and
/// blob), compared for equality only.
pub type Tree<B> = BTreeMap<String, B>;

/// What a path belongs to in the merge.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    List(ProjectName, ListKind),
    /// Open or filed, under any slug.
    Brief(ProjectName, BriefId),
    /// Not the board's.
    Other(String),
}

fn key(path: &str) -> Key {
    match BoardPath::parse(path) {
        Some(BoardPath::List { project, kind }) => Key::List(project, kind),
        Some(BoardPath::Brief { project, name, .. }) => Key::Brief(project, name.number()),
        None => Key::Other(path.to_owned()),
    }
}

/// The result of [`merge_trees`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merge<B> {
    /// The merged tree. A list in `lines` still has `ours`' entry here (or
    /// none): the line merge decides what replaces it.
    pub tree: Tree<B>,
    /// The lists both sides changed, to merge by line with [`reconcile`].
    pub lines: Vec<LineMerge<B>>,
    /// The local files of briefs both sides changed: the remote's version
    /// stands in `tree`, these are to be saved beside it.
    pub copies: Vec<LocalCopy<B>>,
}

/// A list file changed on both sides; a side without the file has `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineMerge<B> {
    pub path: String,
    pub kind: ListKind,
    pub base: Option<B>,
    pub ours: Option<B>,
    pub theirs: Option<B>,
}

/// A file of a brief whose local version lost: saved as `<path>.local`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalCopy<B> {
    pub brief: BriefId,
    pub path: String,
    pub blob: B,
}

/// Merges `theirs` (the clone's `main`) into `ours` (the remote's), from
/// their merge base.
///
/// Per key, the paths and entries it has on each side: a key that only one
/// side changed takes that side; a list both changed is merged by line; a
/// brief both changed keeps `ours`, the remote's, and every local file of it
/// that differs becomes a copy. The remote stands because it is what every
/// other clone already has: taking the local version would push a change
/// over theirs, and the local one is kept in the copy, not lost. Rejected:
/// stopping the write until the brief is merged by hand, which blocks every
/// project on the clone with git as the only way out. A path that is not the
/// board's and that both sides changed keeps the remote's too, without a copy:
/// the board does not own it, and the clone's version stays in its history.
pub fn merge_trees<B: Clone + Eq>(base: &Tree<B>, ours: &Tree<B>, theirs: &Tree<B>) -> Merge<B> {
    let (base_keys, our_keys, their_keys) = (by_key(base), by_key(ours), by_key(theirs));
    let none = BTreeMap::new();
    let keys: BTreeSet<&Key> = base_keys
        .keys()
        .chain(our_keys.keys())
        .chain(their_keys.keys())
        .collect();
    let mut merge = Merge {
        tree: ours.clone(),
        lines: Vec::new(),
        copies: Vec::new(),
    };
    for key in keys {
        let (base, ours, theirs) = (
            base_keys.get(key).unwrap_or(&none),
            our_keys.get(key).unwrap_or(&none),
            their_keys.get(key).unwrap_or(&none),
        );
        if theirs == base || theirs == ours {
            continue;
        }
        if ours == base {
            for path in ours.keys() {
                merge.tree.remove(path);
            }
            merge.tree.extend(
                theirs
                    .iter()
                    .map(|(path, entry)| (path.clone(), entry.clone())),
            );
            continue;
        }
        match key {
            Key::List(project, kind) => {
                // A list key has one path; the grammar has one per list.
                let entry = |side: &BTreeMap<String, B>| side.values().next().cloned();
                merge.lines.push(LineMerge {
                    path: BoardPath::List {
                        project: project.clone(),
                        kind: *kind,
                    }
                    .to_string(),
                    kind: *kind,
                    base: entry(base),
                    ours: entry(ours),
                    theirs: entry(theirs),
                });
            }
            Key::Brief(_, number) => {
                merge.copies.extend(
                    theirs
                        .iter()
                        .filter(|(path, entry)| ours.get(*path) != Some(entry))
                        .map(|(path, entry)| LocalCopy {
                            brief: *number,
                            path: path.clone(),
                            blob: entry.clone(),
                        }),
                );
            }
            Key::Other(_) => {}
        }
    }
    merge
}

fn by_key<B: Clone>(tree: &Tree<B>) -> BTreeMap<Key, BTreeMap<String, B>> {
    let mut keys: BTreeMap<Key, BTreeMap<String, B>> = BTreeMap::new();
    for (path, entry) in tree {
        keys.entry(key(path))
            .or_default()
            .insert(path.clone(), entry.clone());
    }
    keys
}

/// What a line of a list is to [`reconcile`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum LineKey<'a> {
    /// An item whose id names one line in each version.
    Id(u32),
    /// Whitespace only.
    Blank,
    /// Everything else, by its content: headings, free text, indented lines,
    /// items without an id, and items whose id is ambiguous.
    Text(&'a [u8]),
}

fn line_key<'a>(kind: ListKind, ambiguous: &BTreeSet<u32>, line: &'a [u8]) -> LineKey<'a> {
    let line = raw::content(line);
    if raw::is_blank(line) {
        return LineKey::Blank;
    }
    match parse_line(kind, line) {
        Entry::Item { id: Some(n), .. } if !ambiguous.contains(&n) => LineKey::Id(n),
        Entry::Item { .. } | Entry::Heading(_) | Entry::Text(_) => LineKey::Text(line),
    }
}

/// Per id of a version, the indented lines below its item, as
/// [`is_continuation`] reads them.
fn indented_below<'a>(
    kind: ListKind,
    ambiguous: &BTreeSet<u32>,
    lines: &[&'a [u8]],
) -> BTreeMap<u32, Vec<&'a [u8]>> {
    let mut below: BTreeMap<u32, Vec<&'a [u8]>> = BTreeMap::new();
    let mut owner = None;
    for line in lines {
        owner = match (owner, line_key(kind, ambiguous, line)) {
            (Some(n), LineKey::Text(text)) if is_continuation(text) => {
                below.entry(n).or_default().push(line);
                Some(n)
            }
            (_, LineKey::Id(n)) => Some(n),
            (_, LineKey::Blank | LineKey::Text(_)) => None,
        };
    }
    below
}

/// Of an item's part (its line, its indented lines) on the three sides, the
/// one to keep: the clone's when the clone changed it, else the remote's.
/// `None` for a side without the item; the item is on at least one.
fn pick<T: Copy>(
    base: Option<T>,
    ours: Option<T>,
    theirs: Option<T>,
    same: impl Fn(T, T) -> bool,
) -> T {
    match (theirs, ours) {
        (Some(t), _) if base.is_none_or(|b| !same(t, b)) => t,
        (_, Some(o)) => o,
        (Some(t), None) => t,
        (None, None) => unreachable!("an expected id is on a side"),
    }
}

fn same_contents(a: &[&[u8]], b: &[&[u8]]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(a, b)| raw::content(a) == raw::content(b))
}

/// One version of a list: its lines, raw.
struct Version<'a> {
    lines: Vec<&'a [u8]>,
}

impl<'a> Version<'a> {
    fn new(bytes: &'a [u8]) -> Version<'a> {
        Version {
            lines: raw::lines(bytes).collect(),
        }
    }

    /// The raw lines of each id.
    fn ids(&self, kind: ListKind) -> BTreeMap<u32, Vec<&'a [u8]>> {
        let mut ids: BTreeMap<u32, Vec<&'a [u8]>> = BTreeMap::new();
        for line in &self.lines {
            if let Entry::Item { id: Some(n), .. } = parse_line(kind, raw::content(line)) {
                ids.entry(n).or_default().push(line);
            }
        }
        ids
    }
}

/// The merged bytes of a list file, from its three versions and their union
/// (`git merge-file --union`, which gives the line order).
///
/// `ours` is the remote's version, `theirs` the clone's. An item is keyed by
/// its id, so a moved or edited line is the same item:
///
/// - expected: an id the base had stays when both sides still have it (a
///   delete beats an edit and a move); a new id stays when either side has
///   it;
/// - its line: the clone's when the clone changed it, else the remote's (an
///   edit against an edit is a human race; the local one wins);
/// - each expected id once, at the first union line holding its line, else
///   at the end, and directly below it the indented lines it is to have.
///
/// An id is ambiguous when a version has it on two lines, or when both sides
/// added it with different text: only a hand-typed id gets there, and which
/// line it was meant for is a guess, so all its lines are kept as text and
/// lint reports them. Text (headings, free text, indented lines, items
/// without an id) is counted three-way: as often as the remote's and the
/// clone's have it, less the base's, so a line deleted on one side goes and
/// a line added on both is kept twice. Blank lines are kept as the union has
/// them.
///
/// An item's indented lines are text by that count, but they are put where
/// the item is: the union puts lines where its hunks fall, so an item moved
/// on one side and edited on the other comes out of it in two places, its
/// indented lines under the wrong one. So of an expected item, its indented
/// lines on the side that changed them (as for its line) go directly below
/// it, as far as the count allows, and have the first claim on that count;
/// the union's other text takes what is left, where the union has it.
/// Whether an indented line stays is the count's, never the item's: `board
/// done` and `drop` delete only the item line, and the indented lines left
/// then read as the item above's, on one side and not the other, so a rule
/// by item would lose an edit of them made on the other side. The cost: an
/// item deleted with its indented lines on one side keeps the ones the other
/// side edited, as text below the item above. Rejected: merging indented
/// lines line by line within the item, which needs an order of their own
/// for what is mostly one line. Indented lines as they stood with a line of
/// the union are put in with its bytes, as that line is.
///
/// For any `union` the ids come out right; only the order of lines and blank
/// lines depend on it.
pub fn reconcile(kind: ListKind, base: &[u8], ours: &[u8], theirs: &[u8], union: &[u8]) -> Vec<u8> {
    let versions = [base, ours, theirs].map(Version::new);
    let [base_ids, our_ids, their_ids] =
        [&versions[0], &versions[1], &versions[2]].map(|version| version.ids(kind));
    let content = |line: &[u8]| raw::content(line).to_vec();

    let ambiguous: BTreeSet<u32> = [&base_ids, &our_ids, &their_ids]
        .into_iter()
        .flat_map(|ids| ids.keys())
        .copied()
        .filter(|n| {
            let twice = [&base_ids, &our_ids, &their_ids]
                .iter()
                .any(|ids| ids.get(n).is_some_and(|lines| lines.len() > 1));
            let added_apart = !base_ids.contains_key(n)
                && match (our_ids.get(n), their_ids.get(n)) {
                    (Some(ours), Some(theirs)) => content(ours[0]) != content(theirs[0]),
                    _ => false,
                };
            twice || added_apart
        })
        .collect();
    let key = |line| line_key(kind, &ambiguous, line);
    let under = versions
        .each_ref()
        .map(|version| indented_below(kind, &ambiguous, &version.lines));

    // Per id, the line and the indented lines it is to have, when it is to
    // stay.
    let mut wanted: BTreeMap<u32, (&[u8], &[&[u8]])> = BTreeMap::new();
    for n in our_ids.keys().chain(their_ids.keys()) {
        if ambiguous.contains(n) {
            continue;
        }
        let (b, o, t) = (base_ids.get(n), our_ids.get(n), their_ids.get(n));
        let expected = match b {
            Some(_) => o.is_some() && t.is_some(),
            None => o.is_some() || t.is_some(),
        };
        if !expected {
            continue;
        }
        let line = pick(b.map(|l| l[0]), o.map(|l| l[0]), t.map(|l| l[0]), |x, y| {
            raw::content(x) == raw::content(y)
        });
        let below = |side: Option<&Vec<&[u8]>>, v: usize| {
            side.map(|_| under[v].get(n).map_or(&[][..], Vec::as_slice))
        };
        let below = pick(below(b, 0), below(o, 1), below(t, 2), same_contents);
        wanted.insert(*n, (line, below));
    }

    let mut budget = Budget::default();
    let count = |version: &Version<'_>, text: &[u8]| {
        version
            .lines
            .iter()
            .filter(|line| line_key(kind, &ambiguous, line) == LineKey::Text(text))
            .count()
    };
    for version in &versions[1..] {
        for line in &version.lines {
            if let LineKey::Text(text) = line_key(kind, &ambiguous, line) {
                budget.allowed.entry(text).or_insert_with(|| {
                    (count(&versions[1], text) + count(&versions[2], text))
                        .saturating_sub(count(&versions[0], text))
                });
            }
        }
    }
    for line in wanted.values().flat_map(|(_, below)| *below) {
        *budget.reserved.entry(raw::content(line)).or_default() += 1;
    }

    let union: Vec<&[u8]> = raw::lines(union).collect();
    let terminator = raw::terminator_of(union.first().or(versions[1].lines.first()).copied());
    let mut out: Vec<u8> = Vec::with_capacity(union.iter().map(|line| line.len()).sum());
    let mut push = |lines: &[&[u8]]| {
        for line in lines {
            if !out.is_empty() && !out.ends_with(b"\n") {
                out.extend_from_slice(terminator);
            }
            out.extend_from_slice(line);
        }
    };
    let mut emitted: BTreeSet<u32> = BTreeSet::new();
    let mut at = 0;
    while let Some(line) = union.get(at).copied() {
        at += 1;
        match key(line) {
            LineKey::Blank => push(&[line]),
            LineKey::Id(n) => {
                if let Some((want, under)) = wanted.get(&n)
                    && !emitted.contains(&n)
                    && raw::content(line) == raw::content(want)
                {
                    emitted.insert(n);
                    push(&[line]);
                    // The union's indented lines here when they are the
                    // ones wanted; else they are candidates as text.
                    let run = union[at..]
                        .iter()
                        .take_while(|line| is_continuation(raw::content(line)))
                        .count();
                    let under = if same_contents(&union[at..at + run], under) {
                        at += run;
                        &union[at - run..at]
                    } else {
                        under
                    };
                    for line in under {
                        if budget.below(raw::content(line)) {
                            push(&[line]);
                        }
                    }
                }
            }
            LineKey::Text(text) => {
                if budget.loose(text) {
                    push(&[line]);
                }
            }
        }
    }
    for (n, (line, under)) in &wanted {
        if !emitted.contains(n) {
            push(&[line]);
            for line in *under {
                if budget.below(raw::content(line)) {
                    push(&[line]);
                }
            }
        }
    }
    out
}

/// How often [`reconcile`] may still put in each text: its three-way count,
/// of which the lines wanted below an item have first claim.
#[derive(Default)]
struct Budget<'a> {
    allowed: BTreeMap<&'a [u8], usize>,
    /// Lines wanted below an item, not yet come to.
    reserved: BTreeMap<&'a [u8], usize>,
    emitted: BTreeMap<&'a [u8], usize>,
}

impl<'a> Budget<'a> {
    /// Whether a line wanted below an item goes in.
    fn below(&mut self, text: &'a [u8]) -> bool {
        if let Some(reserved) = self.reserved.get_mut(text) {
            *reserved = reserved.saturating_sub(1);
        }
        self.take(text, 0)
    }

    /// Whether a line of the union outside the items goes in.
    fn loose(&mut self, text: &'a [u8]) -> bool {
        let reserved = self.reserved.get(text).copied().unwrap_or(0);
        self.take(text, reserved)
    }

    fn take(&mut self, text: &'a [u8], keep: usize) -> bool {
        let allowed = self.allowed.get(text).copied().unwrap_or(0);
        let emitted = self.emitted.entry(text).or_insert(0);
        let fits = *emitted + keep < allowed;
        *emitted += usize::from(fits);
        fits
    }
}

/// An id the remote's side gave an item the base has without one: the
/// item's line as the base has it, and the id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    pub line: Vec<u8>,
    pub id: u32,
}

/// The ids that the commits after the base, oldest first, gave the base's
/// items without an id.
///
/// Such an item is a hand edit of this clone, published under an id by one
/// of its own pushes while its files still show it without one: killed after
/// the push, or saved again while it ran. Without the id, [`reconcile`]
/// cannot tell the published line is that item: it reads as one the remote
/// added, and a delete or move of it here is undone. So the id is read where
/// it was given, at the first version that has fewer of the line without an
/// id than the base, and not from the remote's file as it is now, where
/// another clone may have edited the line since. Among the lines that carry
/// it there, an id new in that version goes first: another clone may have
/// added the same text under an id of its own before.
pub fn published_ids<'a>(
    kind: ListKind,
    base: &[u8],
    later: impl IntoIterator<Item = &'a [u8]>,
) -> Vec<Published> {
    let base_ids: BTreeSet<u32> = Version::new(base).ids(kind).into_keys().collect();
    let mut pending: BTreeMap<&[u8], usize> = BTreeMap::new();
    for line in Version::new(base).lines {
        if is_id_less(kind, line) {
            *pending.entry(raw::content(line)).or_default() += 1;
        }
    }
    let mut published = Vec::new();
    let mut given: BTreeSet<u32> = BTreeSet::new();
    let mut before = base_ids.clone();
    for version in later {
        if pending.values().all(|left| *left == 0) {
            break;
        }
        let version = Version::new(version);
        let ids = version.ids(kind);
        for (line, left) in &mut pending {
            let still = version
                .lines
                .iter()
                .filter(|l| is_id_less(kind, l) && raw::content(l) == *line)
                .count();
            if still >= *left {
                continue;
            }
            let mut carriers: Vec<(bool, usize, u32)> = ids
                .iter()
                .filter(|(n, lines)| {
                    !base_ids.contains(n)
                        && !given.contains(n)
                        && lines.len() == 1
                        && raw::content(lines[0]) == with_id(kind, line, **n)
                })
                .map(|(n, lines)| {
                    let at = version.lines.iter().position(|l| *l == lines[0]);
                    (before.contains(n), at.unwrap_or(usize::MAX), *n)
                })
                .collect();
            carriers.sort_unstable();
            for (_, _, id) in carriers.into_iter().take(*left - still) {
                given.insert(id);
                published.push(Published {
                    line: line.to_vec(),
                    id,
                });
            }
            *left = still;
        }
        before = ids.into_keys().collect();
    }
    published
}

fn is_id_less(kind: ListKind, line: &[u8]) -> bool {
    matches!(
        parse_line(kind, raw::content(line)),
        Entry::Item { id: None, .. }
    )
}

/// `bytes` with each published id given to an item without an id whose line
/// is the one it was published for, once per id, in file order. Applied to
/// the base and to the clone's version before [`reconcile`], so the item is
/// the same id on all three sides: deleted here, it goes; moved here, it is
/// not doubled.
///
/// An item whose text was changed here too no longer has the line it was
/// published for: the change is kept as an item without an id, which gets
/// a new one, and the published id goes. Its line is not doubled, but a
/// delete of the published id on another clone does not win over that
/// change. Rejected: matching the changed line to the id by position, which
/// needs the hunks of a diff and guesses all the same.
pub fn adopt_ids(kind: ListKind, bytes: &[u8], published: &[Published]) -> Vec<u8> {
    let mut left: Vec<&Published> = published.iter().collect();
    raw::lines(bytes)
        .flat_map(|line| {
            if is_id_less(kind, line)
                && let Some(at) = left
                    .iter()
                    .position(|p| p.line.as_slice() == raw::content(line))
            {
                let id = left.remove(at).id;
                return with_id(kind, line, id);
            }
            line.to_vec()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
    use std::cell::Cell;
    use std::fs;
    use std::process::Command;

    const CHORES: ListKind = ListKind::Chores;

    fn tree(entries: &[(&str, &str)]) -> Tree<String> {
        entries
            .iter()
            .map(|(path, blob)| ((*path).to_owned(), (*blob).to_owned()))
            .collect()
    }

    // ---- merge_trees ----

    #[test]
    fn a_key_changed_on_one_side_takes_that_side() {
        let base = tree(&[
            ("shop/chores.md", "c0"),
            ("shop/backlog.md", "b0"),
            ("shop/briefs/1-a.md", "1"),
            ("README.md", "r0"),
        ]);
        // The remote edited the chores and filed the brief; the clone edited
        // the backlog and the readme and added a brief.
        let ours = tree(&[
            ("shop/chores.md", "c1"),
            ("shop/backlog.md", "b0"),
            ("shop/briefs/done/1-a.md", "1"),
            ("README.md", "r0"),
        ]);
        let theirs = tree(&[
            ("shop/chores.md", "c0"),
            ("shop/backlog.md", "b2"),
            ("shop/briefs/1-a.md", "1"),
            ("shop/briefs/2-b.md", "2"),
            ("README.md", "r2"),
        ]);
        let merge = merge_trees(&base, &ours, &theirs);
        assert_eq!(
            merge.tree,
            tree(&[
                ("shop/chores.md", "c1"),
                ("shop/backlog.md", "b2"),
                ("shop/briefs/done/1-a.md", "1"),
                ("shop/briefs/2-b.md", "2"),
                ("README.md", "r2"),
            ])
        );
        assert_eq!(merge.lines, []);
        assert_eq!(merge.copies, []);
    }

    #[test]
    fn a_key_changed_alike_on_both_sides_is_merged_already() {
        let base = tree(&[("shop/chores.md", "c0")]);
        let both = tree(&[("shop/chores.md", "c1"), ("shop/briefs/3-x.md", "3")]);
        let merge = merge_trees(&base, &both, &both);
        assert_eq!(merge.tree, both);
        assert_eq!((merge.lines, merge.copies), (vec![], vec![]));
    }

    #[test]
    fn a_list_changed_on_both_sides_is_merged_by_line() {
        let base = tree(&[("shop/chores.md", "c0")]);
        let ours = tree(&[("shop/chores.md", "c1")]);
        let theirs = tree(&[]);
        let merge = merge_trees(&base, &ours, &theirs);
        assert_eq!(merge.tree, ours);
        assert_eq!(
            merge.lines,
            [LineMerge {
                path: "shop/chores.md".to_owned(),
                kind: CHORES,
                base: Some("c0".to_owned()),
                ours: Some("c1".to_owned()),
                theirs: None,
            }]
        );
    }

    #[test]
    fn a_brief_changed_differently_on_both_sides_keeps_ours_and_yields_a_copy() {
        let base = tree(&[("shop/briefs/1-a.md", "v0"), ("shop/briefs/2-b.md", "w0")]);
        // The remote edited brief 1; the clone filed it with another edit.
        // Brief 2: the remote deleted it, the clone edited it.
        let ours = tree(&[("shop/briefs/1-a.md", "v1")]);
        let theirs = tree(&[
            ("shop/briefs/done/1-a.md", "v2"),
            ("shop/briefs/2-b.md", "w2"),
        ]);
        let merge = merge_trees(&base, &ours, &theirs);
        assert_eq!(merge.tree, ours);
        assert_eq!(
            merge.copies,
            [
                LocalCopy {
                    brief: BriefId::new(1),
                    path: "shop/briefs/done/1-a.md".to_owned(),
                    blob: "v2".to_owned(),
                },
                LocalCopy {
                    brief: BriefId::new(2),
                    path: "shop/briefs/2-b.md".to_owned(),
                    blob: "w2".to_owned(),
                },
            ]
        );
        // A brief the clone deleted while the remote edited it: no local
        // version to keep.
        let theirs = tree(&[("shop/briefs/2-b.md", "w0")]);
        let merge = merge_trees(&base, &ours, &theirs);
        assert_eq!((merge.tree, merge.copies), (ours, vec![]));
    }

    #[test]
    fn a_file_outside_the_board_changed_on_both_sides_keeps_ours() {
        let base = tree(&[("README.md", "r0")]);
        let ours = tree(&[("README.md", "r1")]);
        let theirs = tree(&[("README.md", "r2")]);
        let merge = merge_trees(&base, &ours, &theirs);
        assert_eq!(merge.tree, ours);
        assert_eq!((merge.lines, merge.copies), (vec![], vec![]));
    }

    fn side() -> impl Strategy<Value = Tree<String>> {
        let path = prop::sample::select(vec![
            "shop/chores.md",
            "shop/backlog.md",
            "shop/briefs/1-a.md",
            "shop/briefs/done/1-a.md",
            "shop/briefs/2-b.md",
            "README.md",
        ]);
        prop::collection::btree_map(path.prop_map(str::to_owned), "[xyz]", 0..6)
    }

    proptest! {
        /// Whatever the trees: an unchanged side never wins over a changed
        /// one, the remote's version of every key is either kept or replaced
        /// whole by the clone's, and a line merge or a copy comes only from a
        /// key both sides changed.
        #[test]
        fn each_key_comes_from_one_side_or_is_merged(
            base in side(), ours in side(), theirs in side(),
        ) {
            let merge = merge_trees(&base, &ours, &theirs);
            let (b, o, t, m) = (by_key(&base), by_key(&ours), by_key(&theirs), by_key(&merge.tree));
            let none = BTreeMap::new();
            let keys: BTreeSet<&Key> = b.keys().chain(o.keys()).chain(t.keys()).collect();
            for key in keys {
                let get = |side: &BTreeMap<Key, BTreeMap<String, String>>| side.get(key).unwrap_or(&none).clone();
                let (b, o, t, m) = (get(&b), get(&o), get(&t), get(&m));
                if o == b {
                    prop_assert_eq!(&m, &t);
                } else {
                    prop_assert_eq!(&m, &o);
                }
                let both_changed = o != b && t != b && o != t;
                let listed = merge.lines.iter().any(|l| key == &self::key(&l.path));
                let copied = merge.copies.iter().any(|c| key == &self::key(&c.path));
                prop_assert_eq!(listed, both_changed && matches!(key, Key::List(..)));
                prop_assert_eq!(copied, both_changed && matches!(key, Key::Brief(..)) && t.iter().any(|(p, e)| o.get(p) != Some(e)));
            }
        }
    }

    // ---- reconcile ----

    fn reconciled(base: &str, ours: &str, theirs: &str, union: &str) -> String {
        String::from_utf8(reconcile(
            CHORES,
            base.as_bytes(),
            ours.as_bytes(),
            theirs.as_bytes(),
            union.as_bytes(),
        ))
        .unwrap()
    }

    #[test]
    fn a_delete_beats_an_edit_and_a_move() {
        let base = "- c1 a\n- c2 b\n- c3 c\n";
        // The remote deleted c3 and moved c1 down; the clone edited c3 and
        // deleted c1.
        let ours = "- c2 b\n- c1 a\n";
        let theirs = "- c2 b\n- c3 c, edited\n";
        let union = "- c2 b\n- c1 a\n- c3 c, edited\n";
        assert_eq!(reconciled(base, ours, theirs, union), "- c2 b\n");
    }

    #[test]
    fn adds_of_both_sides_stay_and_the_local_edit_wins() {
        let base = "## Home\n- c1 a\n";
        let ours = "## Home\n- c1 a, remote\n- c2 new here\n";
        let theirs = "## Home\n- c1 a, local\n- c3 new there\n";
        let union = "## Home\n- c1 a, remote\n- c2 new here\n- c1 a, local\n- c3 new there\n";
        assert_eq!(
            reconciled(base, ours, theirs, union),
            "## Home\n- c2 new here\n- c1 a, local\n- c3 new there\n"
        );
    }

    #[test]
    fn a_moved_line_is_not_doubled() {
        let base = "- c1 a\n- c2 b\n- c3 c\n";
        let ours = "- c3 c\n- c1 a\n- c2 b\n";
        let theirs = "- c1 a\n- c2 b\n- c3 c\n- c4 d\n";
        let union = "- c3 c\n- c1 a\n- c2 b\n- c3 c\n- c4 d\n";
        assert_eq!(
            reconciled(base, ours, theirs, union),
            "- c3 c\n- c1 a\n- c2 b\n- c4 d\n"
        );
    }

    #[test]
    fn text_is_counted_three_way() {
        let base = "# Chores\nnote\n- no id\n";
        // The clone deleted the note and assigned the line an id; the remote
        // added a second note.
        let ours = "# Chores\nnote\n- no id\nnote\n";
        let theirs = "# Chores\n- c9 no id\n";
        let union = "# Chores\nnote\n- no id\nnote\n- c9 no id\n";
        assert_eq!(
            reconciled(base, ours, theirs, union),
            "# Chores\nnote\n- c9 no id\n"
        );
    }

    #[test]
    fn both_lines_of_an_ambiguous_id_are_kept() {
        // c5 typed by hand on both sides for two things, and c6 twice on one.
        let base = "- c1 a\n";
        let ours = "- c1 a\n- c5 remote\n";
        let theirs = "- c1 a\n- c5 local\n- c6 x\n- c6 y\n";
        let union = "- c1 a\n- c5 remote\n- c5 local\n- c6 x\n- c6 y\n";
        assert_eq!(reconciled(base, ours, theirs, union), union);
    }

    #[test]
    fn an_expected_line_missing_from_the_union_is_appended() {
        assert_eq!(
            reconciled("", "- c1 a", "- c2 b\r\n", "junk\n- c1 other"),
            "- c1 a\n- c2 b\r\n"
        );
    }

    /// Lines from a small set, so that ids, texts and blank lines repeat
    /// across versions and within one.
    fn list_line() -> impl Strategy<Value = String> {
        prop_oneof![
            6 => (1..6u32, prop::sample::select(vec!["a", "b", "b edited"]))
                .prop_map(|(n, text)| format!("- c{n} {text}")),
            2 => prop::sample::select(vec!["- no id", "## Home", "note"]).prop_map(String::from),
            2 => prop::sample::select(vec!["  more", "\tless"]).prop_map(String::from),
            1 => prop::sample::select(vec!["", "  "]).prop_map(String::from),
        ]
    }

    fn list_bytes() -> impl Strategy<Value = String> {
        (prop::collection::vec(list_line(), 0..8), any::<bool>()).prop_map(|(lines, crlf)| {
            let end = if crlf { "\r\n" } else { "\n" };
            lines.iter().map(|line| format!("{line}{end}")).collect()
        })
    }

    fn contents(bytes: &str) -> Vec<String> {
        bytes.lines().map(str::to_owned).collect()
    }

    fn id_lines(bytes: &str, n: u32) -> Vec<String> {
        contents(bytes)
            .into_iter()
            .filter(|line| {
                matches!(parse_line(CHORES, line.as_bytes()), Entry::Item { id: Some(m), .. } if m == n)
            })
            .collect()
    }

    /// Per line of `bytes`, the id of the item it is indented below: an
    /// indented line continues the line above it when that is an item with
    /// an id that is not ambiguous, or an indented line that continues one.
    fn owners(bytes: &str, ambiguous: impl Fn(u32) -> bool) -> Vec<Option<u32>> {
        let mut above = None;
        contents(bytes)
            .iter()
            .map(|line| {
                let indented = line.starts_with([' ', '\t']) && !line.trim().is_empty();
                let owner = if indented { above } else { None };
                above = match parse_line(CHORES, line.as_bytes()) {
                    Entry::Item { id: Some(n), .. } if !ambiguous(n) => Some(n),
                    Entry::Item { .. } | Entry::Heading(_) | Entry::Text(_) => owner,
                };
                owner
            })
            .collect()
    }

    /// The rules of `reconcile`, said again the plain way, against any
    /// union: every id that names one line everywhere is there once when
    /// expected, with the line wanted, and gone otherwise. Every other
    /// non-blank line, indented or not, is text with a three-way count.
    /// Directly below each expected item, in the result's order, come the
    /// indented lines wanted below it that the count still allows. A text is
    /// there at most as often as its count, and as the union and the lines
    /// wanted below items have it together; at least as often as its count
    /// and either of the two allows.
    fn check_reconcile(
        base: &str,
        ours: &str,
        theirs: &str,
        union: &str,
    ) -> Result<(), TestCaseError> {
        let result = reconciled(base, ours, theirs, union);
        let versions = [base, ours, theirs];
        let ambiguous = |n: u32| {
            versions.iter().any(|v| id_lines(v, n).len() > 1)
                || (id_lines(base, n).is_empty()
                    && !id_lines(ours, n).is_empty()
                    && !id_lines(theirs, n).is_empty()
                    && id_lines(ours, n) != id_lines(theirs, n))
        };
        let below = |bytes: &str, n: u32| -> Vec<String> {
            contents(bytes)
                .into_iter()
                .zip(owners(bytes, ambiguous))
                .filter(|(_, owner)| *owner == Some(n))
                .map(|(line, _)| line)
                .collect()
        };
        let lines = contents(&result);
        let count = |bytes: &str, text: &str| contents(bytes).iter().filter(|l| *l == text).count();
        let allowed = |text: &str| {
            (count(ours, text) + count(theirs, text)).saturating_sub(count(base, text))
        };
        // Per expected id, where the result has it and the indented lines
        // wanted below it.
        let mut placed: Vec<(usize, Vec<String>)> = Vec::new();
        let ids: BTreeSet<u32> = [base, ours, theirs, union, &result]
            .iter()
            .flat_map(|bytes| contents(bytes))
            .filter_map(|line| match parse_line(CHORES, line.as_bytes()) {
                Entry::Item { id, .. } => id,
                Entry::Heading(_) | Entry::Text(_) => None,
            })
            .collect();
        for n in ids {
            if ambiguous(n) {
                continue;
            }
            let (b, o, t) = (id_lines(base, n), id_lines(ours, n), id_lines(theirs, n));
            let expected = if b.is_empty() {
                !o.is_empty() || !t.is_empty()
            } else {
                !o.is_empty() && !t.is_empty()
            };
            let got = id_lines(&result, n);
            if expected {
                let want = if !t.is_empty() && (b.is_empty() || t != b) {
                    &t
                } else {
                    &o
                };
                prop_assert_eq!(&got, want, "c{}", n);
                let (b, o, t) = (below(base, n), below(ours, n), below(theirs, n));
                let side = |lines: &Vec<String>| !lines.is_empty();
                let want = if side(&id_lines(theirs, n)) && (!side(&id_lines(base, n)) || t != b) {
                    t
                } else {
                    o
                };
                let at = lines.iter().position(|line| *line == got[0]).unwrap();
                placed.push((at, want));
            } else {
                prop_assert_eq!(got, Vec::<String>::new(), "c{} is not expected", n);
            }
        }
        placed.sort();
        let mut below_items: BTreeMap<&str, usize> = BTreeMap::new();
        for line in placed.iter().flat_map(|(_, want)| want) {
            *below_items.entry(line).or_default() += 1;
        }
        let mut put: BTreeMap<&str, usize> = BTreeMap::new();
        for (at, want) in &placed {
            let fits: Vec<&String> = want
                .iter()
                .filter(|line| {
                    let put = put.entry(line).or_default();
                    let fits = *put < allowed(line);
                    *put += usize::from(fits);
                    fits
                })
                .collect();
            let end = (at + 1 + fits.len()).min(lines.len());
            let got: Vec<&String> = lines[at + 1..end].iter().collect();
            prop_assert_eq!(got, fits, "below {:?}: {:?}", &lines[*at], result);
        }
        let is_text = |line: &String| {
            !line.trim().is_empty()
                && match parse_line(CHORES, line.as_bytes()) {
                    Entry::Item { id: Some(n), .. } => ambiguous(n),
                    Entry::Item { .. } | Entry::Heading(_) | Entry::Text(_) => true,
                }
        };
        let texts: Vec<String> = contents(union)
            .into_iter()
            .chain(lines.clone())
            .chain(below_items.keys().map(|line| (*line).to_owned()))
            .collect();
        for text in texts.iter().filter(|l| is_text(l)) {
            let (allowed, in_union) = (allowed(text), count(union, text));
            let below = below_items.get(text.as_str()).copied().unwrap_or(0);
            let got = count(&result, text);
            prop_assert!(
                allowed.min(in_union.max(below)) <= got && got <= allowed.min(in_union + below),
                "{:?} {} times, {} allowed, {} in the union, {} wanted below items: {:?}",
                text,
                got,
                allowed,
                in_union,
                below,
                result
            );
        }
        Ok(())
    }

    proptest! {
        #[test]
        fn reconcile_keeps_its_rules_for_any_union(
            base in list_bytes(), ours in list_bytes(), theirs in list_bytes(), union in list_bytes(),
        ) {
            check_reconcile(&base, &ours, &theirs, &union)?;
        }

        /// A change on one side only is that side, byte for byte.
        #[test]
        fn one_side_changed_gives_that_side(x in list_bytes(), y in list_bytes()) {
            prop_assert_eq!(reconciled(&x, &x, &y, &y), y.clone());
            prop_assert_eq!(reconciled(&x, &y, &x, &y), y);
        }
    }

    // ---- ids published for the base's items without one ----

    fn published(base: &str, later: &[&str]) -> Vec<(String, u32)> {
        published_ids(CHORES, base.as_bytes(), later.iter().map(|v| v.as_bytes()))
            .into_iter()
            .map(|p| (String::from_utf8(p.line).unwrap(), p.id))
            .collect()
    }

    #[test]
    fn a_published_id_is_read_where_it_was_given() {
        let base = "- c1 a\n- foo\n- bar\n";
        let later = [
            // A merge with another clone's line of the same text.
            "- c1 a\n- foo\n- bar\n- c7 foo\n",
            // Ids given; c7 was there before.
            "- c1 a\n- c8 foo\n- c9 bar\n- c7 foo\n",
            // Edited and deleted afterwards on the remote.
            "- c1 a\n- c8 foo, edited\n- c7 foo\n",
        ];
        assert_eq!(
            published(base, &later),
            [("- bar".to_owned(), 9), ("- foo".to_owned(), 8)]
        );
        // Nothing given, or nothing to give.
        assert_eq!(published(base, &[base]), []);
        assert_eq!(published("- c1 a\n", &["- c2 b\n"]), []);
    }

    #[test]
    fn with_its_published_id_a_line_deleted_or_moved_here_stays_so() {
        let base = "- c1 a\n- foo\n- bar\n";
        // The remote gave the ids and then edited c2; the clone, its files
        // still without the ids, deleted foo and moved bar up.
        let ours = "- c1 a\n- c2 foo, edited\n- c3 bar\n";
        let theirs = "- bar\n- c1 a\n";
        let given = published(base, &["- c1 a\n- c2 foo\n- c3 bar\n", ours]);
        let given: Vec<Published> = given
            .into_iter()
            .map(|(line, id)| Published {
                line: line.into_bytes(),
                id,
            })
            .collect();
        let adopt =
            |bytes: &str| String::from_utf8(adopt_ids(CHORES, bytes.as_bytes(), &given)).unwrap();
        let (base, theirs) = (adopt(base), adopt(theirs));
        assert_eq!(theirs, "- c3 bar\n- c1 a\n");
        let union = "- c3 bar\n- c1 a\n- c2 foo, edited\n- c3 bar\n";
        assert_eq!(
            reconciled(&base, ours, &theirs, union),
            "- c3 bar\n- c1 a\n"
        );
    }

    proptest! {
        /// What normalise writes is what adopting its ids gives back.
        #[test]
        fn adopting_the_ids_normalise_gave_gives_its_file(base in list_bytes(), mark in 0..10u32) {
            let mut file = crate::list::ListFile::parse(CHORES, base.as_bytes());
            file.assign_ids(mark).unwrap();
            let normalised = file.to_bytes();
            let given = published_ids(CHORES, base.as_bytes(), [normalised.as_slice()]);
            prop_assert_eq!(adopt_ids(CHORES, base.as_bytes(), &given), normalised);
        }
    }

    // ---- with git's union ----

    /// An item list and the edits two machines make to it: ids 1 to 8 in the
    /// base, the odd ones with an indented line below, new ids from 20 (the
    /// remote) and 40 (the clone), so that only the edits decide what is
    /// expected. An item is its line and the indented lines below it, and
    /// every edit but `Close` takes them as one, as `board move` and an
    /// editor do.
    #[derive(Debug, Clone)]
    enum Edit {
        Delete(prop::sample::Index),
        /// Only the item's line deleted, as `board done` and `board drop`
        /// do: its indented lines then continue the item above, or open the
        /// file.
        Close(prop::sample::Index),
        Change(prop::sample::Index),
        /// The item's first indented line changed, or one added.
        Note(prop::sample::Index),
        Move(prop::sample::Index, prop::sample::Index),
        Add(prop::sample::Index),
    }

    fn edits() -> impl Strategy<Value = Vec<Edit>> {
        let index = any::<prop::sample::Index>;
        prop::collection::vec(
            prop_oneof![
                3 => index().prop_map(Edit::Delete),
                2 => index().prop_map(Edit::Close),
                2 => index().prop_map(Edit::Change),
                2 => index().prop_map(Edit::Note),
                3 => (index(), index()).prop_map(|(a, b)| Edit::Move(a, b)),
                2 => index().prop_map(Edit::Add),
            ],
            0..4,
        )
    }

    /// The items, each its line and its indented lines.
    type Items = Vec<Vec<String>>;

    fn apply(items: &Items, edits: &[Edit], first_new: u32, mark: &str) -> Items {
        let mut items = items.clone();
        let mut next = first_new;
        for edit in edits {
            match edit {
                Edit::Delete(at) if !items.is_empty() => {
                    items.remove(at.index(items.len()));
                }
                Edit::Close(at) if !items.is_empty() => {
                    let at = at.index(items.len());
                    let notes = items.remove(at).split_off(1);
                    match at.checked_sub(1) {
                        Some(above) => items[above].extend(notes),
                        None if !notes.is_empty() => items.insert(0, notes),
                        None => {}
                    }
                }
                Edit::Change(at) if !items.is_empty() => {
                    let at = at.index(items.len());
                    items[at][0] = format!("{} {mark}", items[at][0]);
                }
                Edit::Note(at) if !items.is_empty() => {
                    let at = at.index(items.len());
                    let item = &mut items[at];
                    match item.get_mut(1) {
                        Some(note) => *note = format!("{note} {mark}"),
                        None => item.push(format!("  note {mark}")),
                    }
                }
                Edit::Move(from, to) if !items.is_empty() => {
                    let item = items.remove(from.index(items.len()));
                    items.insert(to.index(items.len() + 1), item);
                }
                Edit::Add(at) => {
                    let item = vec![format!("- c{next} new {mark}")];
                    items.insert(at.index(items.len() + 1), item);
                    next += 1;
                }
                Edit::Delete(_)
                | Edit::Close(_)
                | Edit::Change(_)
                | Edit::Note(_)
                | Edit::Move(..) => {}
            }
        }
        items
    }

    fn joined(items: &Items) -> String {
        items
            .iter()
            .flatten()
            .map(|line| format!("{line}\n"))
            .collect()
    }

    /// `git merge-file -p --union ours base theirs`, on files.
    fn git_union(dir: &std::path::Path, base: &str, ours: &str, theirs: &str) -> String {
        for (name, text) in [("base", base), ("ours", ours), ("theirs", theirs)] {
            fs::write(dir.join(name), text).unwrap();
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "the test compares reconcile with the union git gives"
        )]
        let output = Command::new("git")
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .current_dir(dir)
            .args(["merge-file", "-p", "--union", "ours", "base", "theirs"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    /// `board move` on both sides, one of them offline: the same chore put
    /// into another group on each, and the other side's chore moved too. On
    /// git's union every item is there once; where the item ends up is the
    /// union's order, which only an offline move meets.
    #[test]
    fn a_chore_moved_into_other_groups_on_both_sides_is_there_once() {
        let dir = tempfile::tempdir().unwrap();
        let base = "## Home\n- c1 a\n- c2 b\n## Work\n- c3 c\n- c4 d\n";
        let ours = "## Home\n- c2 b\n## Work\n- c3 c\n- c1 a\n- c4 d\n";
        let theirs = "## Home\n- c4 d\n- c2 b\n- c1 a\n## Work\n- c3 c\n";
        let union = git_union(dir.path(), base, ours, theirs);
        check_reconcile(base, ours, theirs, &union).unwrap();
        let result = reconciled(base, ours, theirs, &union);
        for n in 1..=4 {
            assert_eq!(id_lines(&result, n).len(), 1, "c{n} in {result:?}");
        }
        assert_eq!(result.matches("## ").count(), 2, "{result:?}");
    }

    /// The two ways the union separated an item from its indented line,
    /// before `reconcile` kept them as one: a move here against an edit of
    /// the item there, either way round. The indented line stays under its
    /// item, and no other item gets it.
    #[test]
    fn a_moved_item_keeps_its_indented_line_against_an_edit() {
        let dir = tempfile::tempdir().unwrap();
        let base = "## Home\n- c2 b\n- c1 a\n  about a\n- c3 c\n";
        for (ours, theirs, want) in [
            // The remote edited c1's line; the clone moved c1 offline.
            (
                "## Home\n- c2 b\n- c1 A\n  about a\n- c3 c\n",
                "## Home\n- c2 b\n- c3 c\n- c1 a\n  about a\n",
                "## Home\n- c2 b\n- c1 A\n  about a\n- c3 c\n",
            ),
            // The remote moved c1; the clone edited its indented line.
            (
                "## Home\n- c1 a\n  about a\n- c2 b\n- c3 c\n",
                "## Home\n- c2 b\n- c1 a\n  about A\n- c3 c\n",
                "## Home\n- c1 a\n  about A\n- c2 b\n- c3 c\n",
            ),
        ] {
            let union = git_union(dir.path(), base, ours, theirs);
            check_reconcile(base, ours, theirs, &union).unwrap();
            assert_eq!(reconciled(base, ours, theirs, &union), want, "{union:?}");
        }
    }

    /// `board done` leaves the item's indented line, which then continues
    /// the item above (or opens the group); the other side edited it. The
    /// edit stays, and the old text goes, either way round.
    #[test]
    fn an_edited_indented_line_survives_its_item_closed_on_the_other_side() {
        let dir = tempfile::tempdir().unwrap();
        for (base, closed, edited, want) in [
            (
                "- c2 b\n- c1 a\n  about a\n- c3 c\n",
                "- c2 b\n  about a\n- c3 c\n",
                "- c2 b\n- c1 a\n  about A\n- c3 c\n",
                "- c2 b\n  about A\n- c3 c\n",
            ),
            (
                "## Home\n- c1 a\n  about a\n- c3 c\n",
                "## Home\n  about a\n- c3 c\n",
                "## Home\n- c1 a\n  about A\n- c3 c\n",
                "## Home\n  about A\n- c3 c\n",
            ),
        ] {
            for (ours, theirs) in [(closed, edited), (edited, closed)] {
                let union = git_union(dir.path(), base, ours, theirs);
                check_reconcile(base, ours, theirs, &union).unwrap();
                assert_eq!(reconciled(base, ours, theirs, &union), want, "{union:?}");
            }
        }
    }

    /// The indented lines below the first line of `bytes` that is `line`.
    fn below_line(bytes: &str, line: &str) -> Option<Vec<String>> {
        let lines = contents(bytes);
        let at = lines.iter().position(|l| l == line)?;
        Some(
            lines[at + 1..]
                .iter()
                .take_while(|l| l.starts_with(' '))
                .cloned()
                .collect(),
        )
    }

    /// On git's real union: the rules of `reconcile` hold, and the plain
    /// union breaks them often enough that the generator is known to reach
    /// the cases that need `reconcile`: a delete against an edit or a move,
    /// which brings the deleted line back, and a move against an edit of the
    /// item, which leaves its indented lines below another line than the one
    /// the item is put at.
    #[test]
    fn with_git_s_union_reconcile_keeps_its_rules_and_plain_union_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let (deleted_back, separated) = (Cell::new(0u32), Cell::new(0u32));
        let mut runner = TestRunner::new_with_rng(
            Config {
                cases: 200,
                failure_persistence: None,
                ..Config::default()
            },
            TestRng::deterministic_rng(RngAlgorithm::ChaCha),
        );
        let base: Items = (1..=8)
            .map(|n| {
                let mut item = vec![format!("- c{n} item {n}")];
                if n % 2 == 1 {
                    item.push(format!("  about {n}"));
                }
                item
            })
            .collect();
        let result = runner.run(&(edits(), edits()), |(our_edits, their_edits)| {
            let ours = apply(&base, &our_edits, 20, "remote");
            let theirs = apply(&base, &their_edits, 40, "local");
            let (b, o, t) = (joined(&base), joined(&ours), joined(&theirs));
            let union = git_union(dir.path(), &b, &o, &t);
            check_reconcile(&b, &o, &t, &union)?;
            if (1..=8).any(|n| {
                (id_lines(&o, n).is_empty() || id_lines(&t, n).is_empty())
                    && !id_lines(&union, n).is_empty()
            }) {
                deleted_back.set(deleted_back.get() + 1);
            }
            let result = reconciled(&b, &o, &t, &union);
            if contents(&result).iter().any(|line| {
                line.starts_with('-')
                    && below_line(&union, line)
                        .is_some_and(|run| Some(run) != below_line(&result, line))
            }) {
                separated.set(separated.get() + 1);
            }
            Ok(())
        });
        if let Err(error) = result {
            panic!("{error}");
        }
        assert!(
            deleted_back.get() >= 20 && separated.get() >= 10,
            "plain union broke the delete rule {} times and separated an item from its \
             indented lines {} times",
            deleted_back.get(),
            separated.get()
        );
    }
}
