//! board: a file-based board for a planner/worker pair of coding agents and
//! their user. See SPEC.md.

pub mod board;
pub mod brief;
pub mod error;
mod git;
pub mod id;
pub mod lint;
pub mod list;
mod merge;
mod ops;
pub mod project;
mod raw;
mod sync;

pub use board::{Board, BriefRow, CommitCheck, ListRow, Snapshot};
pub use brief::{
    BadStatus, Brief, EmptyText, EntryText, NotABacklogId, Section, SplitsSection, Stamp, Status,
    Text, Transition, TransitionError,
};
pub use error::{Error, Exit};
pub use id::{BacklogId, BriefId, ChoreId, InvalidId, ItemId, ListKind};
pub use lint::{Finding, Problem, lint};
pub use list::{Entry, FindError, IdsExhausted, Line, ListFile, NoCheck, RemoveError};
pub use ops::{Commits, Filed, NewChore};
pub use project::{BoardPath, BriefFile, BriefName, Env, InvalidSlug, Project, ProjectName, Slug};
pub use sync::Note;
