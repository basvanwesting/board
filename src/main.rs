use std::process::ExitCode;

fn main() -> ExitCode {
    // The commands come with the next slice; until then every invocation is
    // a usage error (exit 2), so nothing can take the binary for a working
    // board.
    eprintln!("board: no commands yet");
    ExitCode::from(2)
}
