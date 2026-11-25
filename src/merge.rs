//! The merge of a clone's own commits (hand edits, writes made offline) with
//! the remote's: which file of which side stands. Pure, so every rule is
//! tested without git; `sync.rs` reads the trees and writes what this
//! decides.
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
//! - A chore or backlog item is one key, its file, named by its id. Changed
//!   on both sides it follows the brief's rule, but for a delete, which wins
//!   either way round (below). Rejected: a field-by-field merge of an item
//!   changed on two machines (one rule for every file, and only an offline
//!   write meets it).

use std::collections::{BTreeMap, BTreeSet};

use crate::id::{BriefId, ItemId, ListId};
use crate::project::{BoardPath, ProjectName, is_old_list};

/// A tree as path to entry; `B` is what git has for a path (its mode and
/// blob), compared for equality only.
pub type Tree<B> = BTreeMap<String, B>;

/// What a path belongs to in the merge.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    /// A chore's or backlog item's file.
    Item(ProjectName, ListId),
    /// Open or filed, under any slug.
    Brief(ProjectName, BriefId),
    /// A list file of the storage before item files.
    OldList(String),
    /// Not the board's.
    Other(String),
}

fn key(path: &str) -> Key {
    match BoardPath::parse(path) {
        Some(BoardPath::Item { project, id }) => Key::Item(project, id),
        Some(BoardPath::Brief { project, name, .. }) => Key::Brief(project, name.number()),
        None if is_old_list(path) => Key::OldList(path.to_owned()),
        None => Key::Other(path.to_owned()),
    }
}

/// The result of [`merge_trees`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merge<B> {
    pub tree: Tree<B>,
    /// The local files of briefs and items both sides changed: the remote's
    /// version stands in `tree`, these are to be saved beside it.
    pub copies: Vec<LocalCopy<B>>,
}

/// A file whose local version lost: saved as `<path>.local`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalCopy<B> {
    /// The brief or item it is of.
    pub of: ItemId,
    pub path: String,
    pub blob: B,
}

/// Merges `theirs` (the clone's `main`) into `ours` (the remote's), from
/// their merge base.
///
/// Per key, the paths and entries it has on each side: a key that only one
/// side changed takes that side; a brief or an item both changed keeps
/// `ours`, the remote's, and every local file of it that differs becomes a
/// copy. The remote stands because it is what every other clone already
/// has: taking the local version would push a change over theirs, and the
/// local one is kept in the copy, not lost. Rejected: stopping the write
/// until the file is merged by hand, which blocks every project on the clone
/// with git as the only way out.
///
/// An item deleted on one side and changed on the other is deleted, either
/// way round, and no copy is made (owner, 2026-11-25): a done chore that
/// keeps nagging through lint is worse than an edit that is in the
/// history. A brief keeps its copy then: its text is a record.
///
/// A path that is not the board's and that both sides changed keeps the
/// remote's too, without a copy: the board does not own it, and the clone's
/// version stays in its history. A list file of the older storage the same,
/// but for a delete: the remote's delete (a migration) loses to the clone's
/// change, which `migrate` then folds in.
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
        let copies = |of: ItemId| {
            theirs
                .iter()
                .filter(|(path, entry)| ours.get(*path) != Some(entry))
                .map(move |(path, entry)| LocalCopy {
                    of,
                    path: path.clone(),
                    blob: entry.clone(),
                })
        };
        match key {
            Key::Item(_, id) => {
                if ours.is_empty() || theirs.is_empty() {
                    for path in ours.keys() {
                        merge.tree.remove(path);
                    }
                } else {
                    merge.copies.extend(copies(ItemId::from(*id)));
                }
            }
            Key::Brief(_, number) => merge.copies.extend(copies(ItemId::Brief(*number))),
            // Deleted by the remote (a migration) and changed on the clone
            // (a machine on the old binary, offline): the clone's list
            // stays, so the refusal and `migrate`'s fold-in take it up, not
            // a silent delete. Changed on both: the remote's, as `Other`.
            Key::OldList(_) => {
                if ours.is_empty() {
                    merge.tree.extend(
                        theirs
                            .iter()
                            .map(|(path, entry)| (path.clone(), entry.clone())),
                    );
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{BacklogId, ChoreId};
    use proptest::prelude::*;

    fn tree(entries: &[(&str, &str)]) -> Tree<String> {
        entries
            .iter()
            .map(|(path, blob)| ((*path).to_owned(), (*blob).to_owned()))
            .collect()
    }

    fn chore(number: u32) -> ItemId {
        ItemId::Chore(ChoreId::new(number).unwrap())
    }

    fn backlog(number: u32) -> ItemId {
        ItemId::Backlog(BacklogId::new(number).unwrap())
    }

    #[test]
    fn a_key_changed_on_one_side_takes_that_side() {
        let base = tree(&[
            ("shop/chores/c1.md", "c0"),
            ("shop/backlog/b1.md", "b0"),
            ("shop/briefs/1-a.md", "1"),
            ("README.md", "r0"),
        ]);
        // The remote edited the chore and filed the brief; the clone edited
        // the backlog item and the readme, and added a brief and an item.
        let ours = tree(&[
            ("shop/chores/c1.md", "c1"),
            ("shop/backlog/b1.md", "b0"),
            ("shop/briefs/done/1-a.md", "1"),
            ("README.md", "r0"),
        ]);
        let theirs = tree(&[
            ("shop/chores/c1.md", "c0"),
            ("shop/backlog/b1.md", "b2"),
            ("shop/backlog/b2.md", "new"),
            ("shop/briefs/1-a.md", "1"),
            ("shop/briefs/2-b.md", "2"),
            ("README.md", "r2"),
        ]);
        let merge = merge_trees(&base, &ours, &theirs);
        assert_eq!(
            merge.tree,
            tree(&[
                ("shop/chores/c1.md", "c1"),
                ("shop/backlog/b1.md", "b2"),
                ("shop/backlog/b2.md", "new"),
                ("shop/briefs/done/1-a.md", "1"),
                ("shop/briefs/2-b.md", "2"),
                ("README.md", "r2"),
            ])
        );
        assert_eq!(merge.copies, []);
    }

    #[test]
    fn a_key_changed_alike_on_both_sides_is_merged_already() {
        let base = tree(&[("shop/chores/c1.md", "c0")]);
        let both = tree(&[("shop/chores/c1.md", "c1"), ("shop/briefs/3-x.md", "3")]);
        let merge = merge_trees(&base, &both, &both);
        assert_eq!(merge.tree, both);
        assert_eq!(merge.copies, vec![]);
    }

    #[test]
    fn an_item_changed_on_both_sides_keeps_ours_and_yields_a_copy() {
        let base = tree(&[("shop/chores/c1.md", "v0")]);
        let ours = tree(&[("shop/chores/c1.md", "v1")]);
        let theirs = tree(&[("shop/chores/c1.md", "v2")]);
        let merge = merge_trees(&base, &ours, &theirs);
        assert_eq!(merge.tree, ours);
        assert_eq!(
            merge.copies,
            [LocalCopy {
                of: chore(1),
                path: "shop/chores/c1.md".to_owned(),
                blob: "v2".to_owned(),
            }]
        );
    }

    #[test]
    fn an_old_list_changed_on_the_clone_stays_over_the_remote_s_delete() {
        let base = tree(&[("shop/chores.md", "v0"), ("x/backlog.md", "w0")]);
        // shop: the remote migrated, the clone changed the list; x: the
        // other way round.
        let ours = tree(&[("x/backlog.md", "w1")]);
        let theirs = tree(&[("shop/chores.md", "v1")]);
        let merge = merge_trees(&base, &ours, &theirs);
        assert_eq!(
            merge.tree,
            tree(&[("shop/chores.md", "v1"), ("x/backlog.md", "w1")])
        );
        assert_eq!(merge.copies, vec![]);
    }

    #[test]
    fn an_item_deleted_on_one_side_and_changed_on_the_other_is_gone_without_a_copy() {
        let base = tree(&[("shop/backlog/b1.md", "v0"), ("x/backlog/b2.md", "w0")]);
        // b1: the remote deleted it, the clone edited it; b2 the other way.
        let ours = tree(&[("x/backlog/b2.md", "w1")]);
        let theirs = tree(&[("shop/backlog/b1.md", "v2")]);
        let merge = merge_trees(&base, &ours, &theirs);
        assert_eq!(merge.tree, tree(&[]));
        assert_eq!(merge.copies, []);
        // A brief so is kept as a copy: its text is a record.
        let base = tree(&[("shop/briefs/2-b.md", "w0")]);
        let merge = merge_trees(&base, &tree(&[]), &tree(&[("shop/briefs/2-b.md", "w2")]));
        assert_eq!(merge.copies.len(), 1);
        let _ = backlog(1);
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
                    of: ItemId::Brief(BriefId::new(1)),
                    path: "shop/briefs/done/1-a.md".to_owned(),
                    blob: "v2".to_owned(),
                },
                LocalCopy {
                    of: ItemId::Brief(BriefId::new(2)),
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
        let base = tree(&[("README.md", "r0"), ("shop/chores.md", "c0")]);
        let ours = tree(&[("README.md", "r1"), ("shop/chores.md", "c1")]);
        let theirs = tree(&[("README.md", "r2"), ("shop/chores.md", "c2")]);
        let merge = merge_trees(&base, &ours, &theirs);
        assert_eq!(merge.tree, ours);
        assert_eq!(merge.copies, vec![]);
    }

    fn side() -> impl Strategy<Value = Tree<String>> {
        let path = prop::sample::select(vec![
            "shop/chores/c1.md",
            "shop/backlog/b1.md",
            "shop/briefs/1-a.md",
            "shop/briefs/done/1-a.md",
            "shop/briefs/2-b.md",
            "shop/chores.md",
            "README.md",
        ]);
        prop::collection::btree_map(path.prop_map(str::to_owned), "[xyz]", 0..6)
    }

    proptest! {
        /// Whatever the trees: an unchanged side never wins over a changed
        /// one, the remote's version of every key is kept or replaced whole
        /// by the clone's (or, for an item deleted on either side, gone; for
        /// an old list the remote deleted, the clone's), and
        /// a copy comes only from a key both sides changed.
        #[test]
        fn each_key_comes_from_one_side(base in side(), ours in side(), theirs in side()) {
            let merge = merge_trees(&base, &ours, &theirs);
            let (b, o, t, m) = (by_key(&base), by_key(&ours), by_key(&theirs), by_key(&merge.tree));
            let none = BTreeMap::new();
            let keys: BTreeSet<&Key> = b.keys().chain(o.keys()).chain(t.keys()).collect();
            for key in keys {
                let get = |side: &BTreeMap<Key, BTreeMap<String, String>>| side.get(key).unwrap_or(&none).clone();
                let (b, o, t, m) = (get(&b), get(&o), get(&t), get(&m));
                let both_changed = o != b && t != b && o != t;
                let deleted = o.is_empty() || t.is_empty();
                if o == b {
                    prop_assert_eq!(&m, &t);
                } else if both_changed && deleted && matches!(key, Key::Item(..)) {
                    prop_assert!(m.is_empty());
                } else if both_changed && o.is_empty() && matches!(key, Key::OldList(_)) {
                    prop_assert_eq!(&m, &t);
                } else {
                    prop_assert_eq!(&m, &o);
                }
                let copied = merge.copies.iter().any(|c| key == &self::key(&c.path));
                let kept = match key {
                    Key::Brief(..) => true,
                    Key::Item(..) => !deleted,
                    Key::OldList(_) | Key::Other(_) => false,
                };
                prop_assert_eq!(
                    copied,
                    both_changed && kept && t.iter().any(|(p, e)| o.get(p) != Some(e))
                );
            }
        }
    }
}
