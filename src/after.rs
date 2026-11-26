//! `after:`: a brief, a chore or a backlog item that waits on one other
//! item of its board (owner, 2026-11-26: "they might have a dependency to
//! another item (brief/chore/backlog) more as a linked list").
//!
//! One predecessor, as a chain says a sequence and a row can show one ID;
//! a brief that needs two goes after the later one. Enforced only by
//! `next`, which skips a brief whose target is unsatisfied; on chores and
//! backlog items it is shown, never enforced. So no order of the queue can
//! break a dependency, and `queue --first` needs no check of its own.

use crate::brief::Status;
use crate::id::{BacklogId, ItemId, ListId};
use crate::project::Project;

/// The header key.
pub(crate) const KEY: &str = "after";

/// The target an `after:` value names; `None` for a value that is no id,
/// which lint reports and which holds nothing up.
pub(crate) fn parse(value: &[u8]) -> Option<ItemId> {
    str::from_utf8(value).ok()?.trim().parse().ok()
}

/// The raw `after:` value of an item or brief of the project, when it is
/// there and has one.
fn value_of(project: &Project, id: ItemId) -> Option<&[u8]> {
    match id {
        ItemId::Brief(number) => project
            .briefs()
            .iter()
            .find(|file| file.name.number() == number)?
            .brief
            .get(KEY),
        ItemId::Chore(_) | ItemId::Backlog(_) => {
            let list = list_id(id)?;
            project.list(list.kind()).get(list.get())?.get(KEY)
        }
    }
}

/// What the item or brief `id` waits on, as its file says, satisfied or not.
pub(crate) fn target_of(project: &Project, id: ItemId) -> Option<ItemId> {
    value_of(project, id).and_then(parse)
}

/// Whether a wait on `target` is over: a brief that is reported or filed,
/// or no file with that id on the board (a chore done, a backlog item
/// dropped or closed, a brief deleted by hand, or an id never given: a
/// hand-written `after:` the write-time check never saw, which so holds
/// nothing up). One rule for every kind of target.
pub(crate) fn satisfied(project: &Project, target: ItemId) -> bool {
    match target {
        ItemId::Brief(number) => project
            .briefs()
            .iter()
            .filter(|file| file.name.number() == number)
            .all(|file| matches!(file.status(), Ok(Status::Reported | Status::Filed))),
        ItemId::Chore(_) | ItemId::Backlog(_) => {
            let Some(list) = list_id(target) else {
                return true;
            };
            project.list(list.kind()).get(list.get()).is_none()
        }
    }
}

/// What `id` still waits on: its target while that is unsatisfied; `None`
/// when it waits on nothing. What `ls` and the rows show (`  after <id>`),
/// and what holds a brief back from `next`.
pub(crate) fn waits_on(project: &Project, id: ItemId) -> Option<ItemId> {
    target_of(project, id).filter(|target| !satisfied(project, *target))
}

/// Why `id` may not wait on `target`, the file aside: itself, or a target
/// whose own chain of `after:` leads back to `id`, which would never be
/// claimed, and for a brief a backlog item its `closes:` names, which goes
/// only when the brief is filed: the `closes` it is about to get (a
/// `queue --closes`), else what its file says. `None` when it may.
pub(crate) fn refusal(
    project: &Project,
    id: ItemId,
    target: ItemId,
    closes: Option<&[BacklogId]>,
) -> Option<String> {
    if target == id {
        return Some(format!("{id} cannot wait on itself"));
    }
    if let (ItemId::Brief(number), ItemId::Backlog(item)) = (id, target) {
        let closed = match closes {
            Some(closes) => closes.contains(&item),
            None => project
                .briefs()
                .iter()
                .filter(|file| file.name.number() == number)
                .any(|file| {
                    file.brief
                        .closes()
                        .is_ok_and(|closes| closes.contains(&item))
                }),
        };
        if closed {
            return Some(closing(id, target));
        }
    }
    let mut seen = vec![target];
    let mut at = target;
    while let Some(next) = target_of(project, at) {
        if next == id {
            let chain: Vec<String> = seen.iter().map(ItemId::to_string).collect();
            return Some(format!(
                "a loop: {id} → {} → {id}, which would wait for ever",
                chain.join(" → ")
            ));
        }
        if seen.contains(&next) {
            // A loop that does not pass `id`: lint's to report, not this
            // write's to refuse.
            return None;
        }
        seen.push(next);
        at = next;
    }
    None
}

/// Why a brief may not wait on a backlog item it closes: the item goes only
/// when the brief is filed, so the wait would never be over.
fn closing(id: ItemId, target: ItemId) -> String {
    format!("{id} closes {target}, which goes only when {id} is filed, so it would wait for ever")
}

/// A chore's or backlog item's id as the lists take it.
fn list_id(id: ItemId) -> Option<ListId> {
    match id {
        ItemId::Brief(_) => None,
        ItemId::Chore(chore) => Some(ListId::Chore(chore)),
        ItemId::Backlog(backlog) => Some(ListId::Backlog(backlog)),
    }
}
