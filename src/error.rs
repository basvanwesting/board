//! Why a command did not do what was asked, and the exit code that says so.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The exit codes. Agents branch on them, so each has one meaning:
///
/// | code | meaning |
/// |---|---|
/// | 0 | ok |
/// | 1 | refused by a rule, not found, or lint findings |
/// | 2 | usage, or project unresolved |
/// | 3 | remote needed and not reached |
/// | 4 | git, io or setup failure |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Ok = 0,
    Refused = 1,
    Usage = 2,
    NoRemote = 3,
    Failure = 4,
}

impl From<Exit> for ExitCode {
    fn from(exit: Exit) -> ExitCode {
        ExitCode::from(exit as u8)
    }
}

/// Each variant's text is the whole message, one line, without the `board: `
/// in front.
#[derive(Debug)]
pub enum Error {
    /// A rule of the board says no, or the item is not there:
    /// `<what>: <why>`.
    Refused(String),
    /// The command line or the environment does not say what to do, or on
    /// which project's board.
    Usage(String),
    /// The write needs the remote and did not reach it, or the remote did
    /// not take the change.
    NeedsRemote(String),
    /// The machine is not set up for the board.
    Setup(String),
    /// git could not be run.
    Git(String),
    Io {
        /// `read`, `list`, …
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
}

impl Error {
    pub(crate) fn io(action: &'static str, path: &Path, source: io::Error) -> Error {
        Error::Io {
            action,
            path: path.to_owned(),
            source,
        }
    }

    pub fn exit(&self) -> Exit {
        match self {
            Error::Refused(_) => Exit::Refused,
            Error::Usage(_) => Exit::Usage,
            Error::NeedsRemote(_) => Exit::NoRemote,
            Error::Setup(_) | Error::Git(_) | Error::Io { .. } => Exit::Failure,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Refused(text)
            | Error::Usage(text)
            | Error::NeedsRemote(text)
            | Error::Setup(text)
            | Error::Git(text) => f.write_str(text),
            Error::Io {
                action,
                path,
                source,
            } => write!(f, "cannot {action} {}: {source}", path.display()),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            Error::Refused(_)
            | Error::Usage(_)
            | Error::NeedsRemote(_)
            | Error::Setup(_)
            | Error::Git(_) => None,
        }
    }
}
