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
//!   edit and a move. Rejected: `merge=union`, which brings a deleted line
//!   back when the other side edited or moved it.

use std::collections::{BTreeMap, BTreeSet};

use crate::id::{BriefId, ListKind};
use crate::list::{Entry, parse_line, with_id};
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
    /// Everything else, by its content: headings, free text, items without
    /// an id, and items whose id is ambiguous.
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
/// - each expected id once, at the first union line holding that line, else
///   at the end.
///
/// An id is ambiguous when a version has it on two lines, or when both sides
/// added it with different text: only a hand-typed id gets there, and which
/// line it was meant for is a guess, so all its lines are kept as text and
/// lint reports them. Text (headings, free text, items without an id) is
/// counted three-way: as often as the remote's and the clone's have it,
/// less the base's, so a line deleted on one side goes and a line added on
/// both is kept twice. Blank lines are kept as the union has them.
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

    // Per id, the line it is to have, when it is to stay.
    let mut wanted: BTreeMap<u32, &[u8]> = BTreeMap::new();
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
        let line = match (t, o) {
            (Some(t), _) if b.is_none_or(|b| content(t[0]) != content(b[0])) => t[0],
            (_, Some(o)) => o[0],
            (Some(t), None) => t[0],
            (None, None) => unreachable!("an expected id is on a side"),
        };
        wanted.insert(*n, line);
    }

    let mut allowed: BTreeMap<&[u8], usize> = BTreeMap::new();
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
                allowed.entry(text).or_insert_with(|| {
                    (count(&versions[1], text) + count(&versions[2], text))
                        .saturating_sub(count(&versions[0], text))
                });
            }
        }
    }

    let terminator = raw::terminator_of(
        raw::lines(union)
            .next()
            .or(versions[1].lines.first().copied()),
    );
    let mut out: Vec<u8> = Vec::with_capacity(union.len());
    let mut push = |line: &[u8]| {
        if !out.is_empty() && !out.ends_with(b"\n") {
            out.extend_from_slice(terminator);
        }
        out.extend_from_slice(line);
    };
    let mut emitted: BTreeSet<u32> = BTreeSet::new();
    let mut emitted_text: BTreeMap<&[u8], usize> = BTreeMap::new();
    for line in raw::lines(union) {
        match key(line) {
            LineKey::Blank => push(line),
            LineKey::Id(n) => {
                if let Some(want) = wanted.get(&n)
                    && !emitted.contains(&n)
                    && raw::content(line) == raw::content(want)
                {
                    push(line);
                    emitted.insert(n);
                }
            }
            LineKey::Text(text) => {
                let done = emitted_text.entry(text).or_insert(0);
                if *done < allowed.get(text).copied().unwrap_or(0) {
                    push(line);
                    *done += 1;
                }
            }
        }
    }
    for (n, line) in &wanted {
        if !emitted.contains(n) {
            push(line);
        }
    }
    out
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
            2 => prop::sample::select(vec!["- no id", "## Home", "note", "  more"]).prop_map(String::from),
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

    /// The rules of `reconcile`, said again the plain way, against any
    /// union: every id that names one line everywhere is there once when
    /// expected, with the line wanted, and gone otherwise; every other
    /// non-blank line is there as often as the union and its three-way count
    /// both allow.
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
        for n in 1..6 {
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
            } else {
                prop_assert_eq!(got, Vec::<String>::new(), "c{} is not expected", n);
            }
        }
        let is_text = |line: &String| {
            !line.trim().is_empty()
                && match parse_line(CHORES, line.as_bytes()) {
                    Entry::Item { id: Some(n), .. } => ambiguous(n),
                    Entry::Item { .. } | Entry::Heading(_) | Entry::Text(_) => true,
                }
        };
        let count =
            |bytes: &str, text: &String| contents(bytes).iter().filter(|l| *l == text).count();
        for text in contents(union)
            .iter()
            .chain(&contents(&result))
            .filter(|l| is_text(l))
        {
            let allowed =
                (count(ours, text) + count(theirs, text)).saturating_sub(count(base, text));
            prop_assert_eq!(
                count(&result, text),
                allowed.min(count(union, text)),
                "{:?}",
                text
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
    /// base, new ids from 20 (the remote) and 40 (the clone), so that only
    /// the edits decide what is expected.
    #[derive(Debug, Clone)]
    enum Edit {
        Delete(prop::sample::Index),
        Change(prop::sample::Index),
        Move(prop::sample::Index, prop::sample::Index),
        Add(prop::sample::Index),
    }

    fn edits() -> impl Strategy<Value = Vec<Edit>> {
        let index = any::<prop::sample::Index>;
        prop::collection::vec(
            prop_oneof![
                3 => index().prop_map(Edit::Delete),
                2 => index().prop_map(Edit::Change),
                3 => (index(), index()).prop_map(|(a, b)| Edit::Move(a, b)),
                2 => index().prop_map(Edit::Add),
            ],
            0..4,
        )
    }

    fn apply(lines: &[String], edits: &[Edit], first_new: u32, mark: &str) -> Vec<String> {
        let mut lines = lines.to_vec();
        let mut next = first_new;
        for edit in edits {
            match edit {
                Edit::Delete(at) if !lines.is_empty() => {
                    lines.remove(at.index(lines.len()));
                }
                Edit::Change(at) if !lines.is_empty() => {
                    let at = at.index(lines.len());
                    lines[at] = format!("{} {mark}", lines[at]);
                }
                Edit::Move(from, to) if !lines.is_empty() => {
                    let line = lines.remove(from.index(lines.len()));
                    lines.insert(to.index(lines.len() + 1), line);
                }
                Edit::Add(at) => {
                    lines.insert(at.index(lines.len() + 1), format!("- c{next} new {mark}"));
                    next += 1;
                }
                Edit::Delete(_) | Edit::Change(_) | Edit::Move(..) => {}
            }
        }
        lines
    }

    fn joined(lines: &[String]) -> String {
        lines.iter().map(|line| format!("{line}\n")).collect()
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

    /// On git's real union: the delete rule holds after `reconcile`, and the
    /// plain union breaks it often enough that the generator is known to
    /// reach the cases that need `reconcile` (a delete against an edit or a
    /// move).
    #[test]
    fn with_git_s_union_a_delete_beats_an_edit_and_plain_union_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let broken = Cell::new(0u32);
        let mut runner = TestRunner::new_with_rng(
            Config {
                cases: 200,
                failure_persistence: None,
                ..Config::default()
            },
            TestRng::deterministic_rng(RngAlgorithm::ChaCha),
        );
        let base: Vec<String> = (1..=8).map(|n| format!("- c{n} item {n}")).collect();
        let result = runner.run(&(edits(), edits()), |(our_edits, their_edits)| {
            let ours = apply(&base, &our_edits, 20, "remote");
            let theirs = apply(&base, &their_edits, 40, "local");
            let (b, o, t) = (joined(&base), joined(&ours), joined(&theirs));
            let union = git_union(dir.path(), &b, &o, &t);
            check_reconcile(&b, &o, &t, &union)?;
            // Plain union: an id one side deleted is still there.
            let deleted_back = (1..=8).any(|n| {
                (id_lines(&o, n).is_empty() || id_lines(&t, n).is_empty())
                    && !id_lines(&union, n).is_empty()
            });
            if deleted_back {
                broken.set(broken.get() + 1);
            }
            Ok(())
        });
        if let Err(error) = result {
            panic!("{error}");
        }
        assert!(
            broken.get() >= 20,
            "plain union broke the delete rule {} times",
            broken.get()
        );
    }
}
