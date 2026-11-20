//! board: a file-based board for a planner/worker pair of coding agents and
//! their user. See SPEC.md.

pub mod brief;
pub mod id;
pub mod list;
pub mod project;
mod raw;

pub use brief::{BadStatus, Brief, Stamp, Status, Transition, TransitionError};
pub use id::{BacklogId, BriefId, ChoreId, InvalidId, ItemId, ListKind};
pub use list::{Entry, FindError, IdsExhausted, Line, ListFile, NoCheck, RemoveError};
pub use project::{BoardPath, BriefName, ProjectName};
