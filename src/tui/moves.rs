//! Where a list's rows stand while moves are out or being made: an item
//! moved with `J`/`K` shows at its new place at once, before any file has
//! it, and stays there while its `board move` runs (SPEC.md, TUI).
//!
//! A place is next to another item, as `board move` names it: before the
//! item below, or after the item above. A chore lands in the group of the
//! item it is put next to, so the rows between two headings are a group
//! here as they are in the listing.

use super::state::{ItemKey, Row};
use crate::id::{BacklogId, ChoreId, ListId, ListKind};
use crate::list::{Move, Side};

fn index_of(rows: &[Row], key: &ItemKey) -> Option<usize> {
    rows.iter().position(|row| row.key.as_ref() == Some(key))
}

/// Puts the row of `item` on `side` of the row of `next_to`; nothing
/// changes when either is not among the rows.
pub(crate) fn put(rows: &mut Vec<Row>, item: &ItemKey, side: Side, next_to: &ItemKey) {
    if item == next_to || index_of(rows, next_to).is_none() {
        return;
    }
    let Some(from) = index_of(rows, item) else {
        return;
    };
    let row = rows.remove(from);
    let at = index_of(rows, next_to).expect("looked for above");
    rows.insert(
        match side {
            Side::Before => at,
            Side::After => at + 1,
        },
        row,
    );
}

/// Puts a move that is out on the worker where the files will have it: by
/// id, as the command finds them.
pub(crate) fn put_sent(rows: &mut Vec<Row>, wanted: &Move) {
    if let (Some(item), Some(target)) = (
        unique(rows, wanted.item().get()),
        unique(rows, wanted.target().get()),
    ) {
        put(rows, &item, wanted.side(), &target);
    }
}

/// The item with this id, when it is on one row: what a command can name.
pub(crate) fn unique(rows: &[Row], number: u32) -> Option<ItemKey> {
    let mut found = rows
        .iter()
        .filter_map(|row| row.key.as_ref())
        .filter(|key| key.id() == Some(number));
    match (found.next(), found.next()) {
        (Some(key), None) => Some(key.clone()),
        _ => None,
    }
}

/// The group the row at `at` is in: the headings above it.
fn group_at(rows: &[Row], at: usize) -> usize {
    rows[..at].iter().filter(|row| row.key.is_none()).count()
}

/// The group `item` is in among `rows`.
pub(crate) fn group_of(rows: &[Row], item: &ItemKey) -> Option<usize> {
    index_of(rows, item).map(|at| group_at(rows, at))
}

/// The rows after one press of `J` (`down`) or `K` on `item`: it goes to
/// the next place in that direction, past a heading into the next group;
/// `None` when there is none.
///
/// A place is in a group with another item, or in `home`, the group the
/// item was in when the presses began. Decided here: a group without
/// items is passed over, because `board move` can only put an item next to
/// another, so no command could take the item there; `home` stays a place,
/// so that a run of presses can bring an item back where it was.
pub(crate) fn step(rows: &[Row], item: &ItemKey, down: bool, home: usize) -> Option<Vec<Row>> {
    let from = index_of(rows, item)?;
    let mut rest = rows.to_vec();
    let row = rest.remove(from);
    let filled: Vec<usize> = (0..rest.len())
        .filter(|at| rest[*at].key.is_some())
        .map(|at| group_at(&rest, at))
        .collect();
    let place = |at: &usize| {
        let group = group_at(&rest, *at);
        group == home || filled.contains(&group)
    };
    // `from` in `rest` is where the item is now.
    let to = if down {
        (from + 1..=rest.len()).find(place)
    } else {
        (0..from).rev().find(place)
    }?;
    rest.insert(to, row);
    Some(rest)
}

/// Where `item` stands among `rows`, as a place next to an item: before
/// the one below it, else after the one above, in its group.
pub(crate) fn place_of(rows: &[Row], item: &ItemKey) -> Option<(Side, ItemKey)> {
    let at = index_of(rows, item)?;
    let below = rows.get(at + 1).and_then(|row| row.key.clone());
    let above = at.checked_sub(1).and_then(|above| rows[above].key.clone());
    below
        .map(|below| (Side::Before, below))
        .or_else(|| above.map(|above| (Side::After, above)))
}

/// The `board move` that puts `item` where it stands among `rows`: next to
/// the item directly below or above it. `None` when it has neither in its
/// group's rows (a heading on both sides), so no command can say the place.
pub(crate) fn wanted(rows: &[Row], item: &ItemKey, kind: ListKind) -> Option<Move> {
    let at = index_of(rows, item)?;
    unique(rows, item.id()?)?;
    let id = |row: Option<&Row>| {
        row.and_then(|row| row.key.as_ref())
            .and_then(ItemKey::id)
            .filter(|number| unique(rows, *number).is_some())
    };
    let (side, target) = match (
        id(rows.get(at + 1)),
        id(at.checked_sub(1).map(|a| &rows[a])),
    ) {
        (Some(below), _) => (Side::Before, below),
        (None, Some(above)) => (Side::After, above),
        (None, None) => return None,
    };
    Move::new(list_id(kind, item.id()?)?, side, list_id(kind, target)?).ok()
}

fn list_id(kind: ListKind, number: u32) -> Option<ListId> {
    match kind {
        ListKind::Chores => ChoreId::new(number).map(ListId::Chore),
        ListKind::Backlog => BacklogId::new(number).map(ListId::Backlog),
    }
}
