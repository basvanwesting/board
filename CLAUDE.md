# board

`SPEC.md` is the approved spec: what the tool does and why. A change that makes one of its sentences false rewrites that sentence in the same commit, so the spec stays a description of the tool and not of a plan.

`README.md` describes the tool too, for a first reader: what and how, with SPEC.md as the why. A change that makes one of its sentences false rewrites that sentence in the same commit. Its `board` commands and its status table are checked by tests in `src/main.rs` (`mod docs`).

- **Public repo.** Code, tests, fixtures and docs name no real project, client or organisation; examples use neutral names (`shop`). What is pushed here cannot be taken back.
- **Green** is `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test`.
- **Dependencies: few, and none younger than 24 hours.** Cargo has no release cooldown, and a build runs the build scripts and proc macros of everything in `Cargo.lock`, so the lockfile is reviewed before the first build, not after. After adding or updating a dependency: resolve without building (`cargo generate-lockfile`, `cargo update`), look up the publish date of every version the lock picked or changed, transitive ones included (`https://crates.io/api/v1/crates/<name>/<version>`, field `created_at`), and pin back what is younger than a day (`cargo update <name> --precise <version>`). Default features off, only the features in use. What std does in a few lines (the UTC stamp, the lock, the error type) is written here instead: each crate is one more thing to vet at every update. `Cargo.lock` is committed.
