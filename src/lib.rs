//! board: a file-based board for a planner/worker pair of coding agents and
//! their user. See SPEC.md.

mod after;
pub mod board;
pub mod brief;
pub mod error;
mod git;
mod header;
pub mod id;
pub mod item;
pub mod lint;
pub mod list;
mod merge;
mod migrate;
mod ops;
pub mod project;
mod raw;
mod sync;
pub mod tui;

pub use board::{
    Board, BriefRow, CommitCheck, Fetcher, ListRow, Next, Shown, Snapshot, UnknownShown,
};
pub use brief::{
    BadStatus, Brief, EmptyText, EntryText, NotABacklogId, Section, SplitsSection, Stamp, Status,
    Text, Transition, TransitionError,
};
pub use error::{Error, Exit};
pub use id::{BacklogId, BriefId, ChoreId, InvalidId, ItemId, ListId, ListKind};
pub use item::{Body, DEFAULT_GROUP, Item, Items, Listed, Row};
pub use lint::{Finding, Problem, lint};
pub use list::{Entry, FindError, IdsExhausted, Line};
pub use ops::{CheckChange, Commits, Filed, NewBacklog, NewChore, Queueing};
pub use project::{BoardPath, BriefFile, BriefName, Env, InvalidSlug, Project, ProjectName, Slug};
pub use sync::{Counts, Fetched, Note};
