//! board: a file-based board for a planner/worker pair of coding agents and
//! their user. See SPEC.md.

pub mod board;
pub mod brief;
pub mod error;
mod git;
pub mod id;
pub mod lint;
pub mod list;
mod ops;
pub mod project;
mod raw;
mod sync;

pub use board::{Board, BriefRow, ListRow, Snapshot};
pub use brief::{BadStatus, Brief, Stamp, Status, Transition, TransitionError};
pub use error::{Error, Exit};
pub use id::{BacklogId, BriefId, ChoreId, InvalidId, ItemId, ListKind};
pub use lint::{Finding, Problem, lint};
pub use list::{Entry, FindError, IdsExhausted, Line, ListFile, NoCheck, RemoveError};
pub use ops::NewChore;
pub use project::{BoardPath, BriefFile, BriefName, Env, Project, ProjectName};
pub use sync::Note;
