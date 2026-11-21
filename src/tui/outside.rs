//! The programs the TUI hands work to: the user's `$EDITOR` for `e`, and
//! the clipboard helpers for a yank. With `src/git.rs` the only place that
//! spawns a process (`clippy.toml`); it never spawns git, so every git
//! still gets the surroundings `git.rs` gives it.

use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

use super::state::Edited;

fn command(program: &str) -> Command {
    #[expect(
        clippy::disallowed_methods,
        reason = "the editor and the clipboard helpers, never git: see the module"
    )]
    Command::new(program)
}

/// Runs `editor` on `file`, at `line` (`+N`, which vi, vim, nvim, emacs
/// and nano read), on the terminal, and waits for it.
///
/// Through `sh`, as git runs `GIT_EDITOR`: the variable is a command line
/// (`code --wait`, `nvim -u x`), not a program name. When `EDITOR`
/// is not set, no editor runs: decided here over a fallback to `vi`, which
/// would open an editor the user never chose on a board file.
pub(crate) fn edit(editor: Option<&OsStr>, file: &Path, line: Option<usize>) -> Edited {
    let Some(editor) = editor.filter(|editor| !editor.is_empty()) else {
        return Edited::NotStarted("EDITOR is not set, so e has no editor to open".to_owned());
    };
    let mut script = OsString::from(editor);
    script.push(" \"$@\"");
    let mut command = command("sh");
    // `$0`: what sh calls the editor in its own messages.
    command.arg("-c").arg(script).arg("editor");
    if let Some(line) = line {
        command.arg(format!("+{line}"));
    }
    command.arg(file);
    let shown = editor.to_string_lossy();
    match command.status() {
        Ok(status) if status.success() => Edited::Closed,
        // sh's own: it found no editor to run.
        Ok(status) if matches!(status.code(), Some(126 | 127)) => Edited::NotStarted(format!(
            "cannot run the editor ({shown}): {}",
            exited(status)
        )),
        Ok(status) => Edited::Failed(format!("the editor ({shown}) {}", exited(status))),
        Err(error) => Edited::NotStarted(format!("cannot run sh for the editor: {error}")),
    }
}

fn exited(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exited with {code}"),
        None => "was killed".to_owned(),
    }
}

/// The clipboard helper of this machine: `wl-copy` under Wayland, `pbcopy`
/// on macOS; `None` elsewhere, where OSC 52 is the only way.
///
/// Decided here: `wl-copy` only with `WAYLAND_DISPLAY` set, not whenever it
/// is installed: on a machine reached over ssh it is there and fails at
/// every yank.
fn helper() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("pbcopy")
    } else if std::env::var_os("WAYLAND_DISPLAY").is_some_and(|display| !display.is_empty()) {
        Some("wl-copy")
    } else {
        None
    }
}

/// Gives `text` to the clipboard helper; why that failed, if it did. A
/// helper that is not installed is no failure: OSC 52 has the text too.
///
/// Waited for: both read the text and return (`wl-copy` leaves a process
/// of its own to serve the clipboard), so the wait is short and a yank is
/// done when its key is. Their output goes nowhere, as the screen is the
/// TUI's, and the process `wl-copy` leaves must hold none of this one's.
pub(crate) fn copy(text: &str) -> Option<String> {
    let program = helper()?;
    let spawned = command(program)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => return Some(format!("cannot run {program}: {error}")),
    };
    let written = child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(text.as_bytes());
    match (written, child.wait()) {
        (_, Err(error)) => Some(format!("{program}: {error}")),
        (_, Ok(status)) if !status.success() => Some(format!("{program} {}", exited(status))),
        (Err(error), Ok(_)) => Some(format!("{program}: {error}")),
        (Ok(()), Ok(_)) => None,
    }
}

/// The terminal's own clipboard: OSC 52 asks the terminal to set it, also
/// over ssh, where no helper reaches the screen the user sits at.
pub(crate) fn osc52(text: &str) -> Vec<u8> {
    [b"\x1b]52;c;", base64(text.as_bytes()).as_bytes(), b"\x07"].concat()
}

/// Standard base64 with padding: a dozen lines, where a crate would be one
/// more thing to vet at every update (CLAUDE.md).
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (at, byte)| {
            n | (u32::from(*byte) << (16 - 8 * at))
        });
        for at in 0..4 {
            if at <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * at)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_is_the_standard_one() {
        // RFC 4648, section 10.
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(plain.as_bytes()), encoded, "{plain}");
        }
        assert_eq!(base64("ü".as_bytes()), "w7w=");
        assert_eq!(osc52("c12"), b"\x1b]52;c;YzEy\x07");
    }
}
