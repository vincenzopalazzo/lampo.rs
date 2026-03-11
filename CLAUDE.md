# CLAUDE.md

## Build & Test

- `make fmt` — Run linting and formatting (rustfmt + clippy)
- `make check` — Run all tests
- `cargo check -p <crate>` — Type-check a single crate
- `cargo test -p <crate>` — Test a single crate

## Code Style

- Follow Rust standard formatting (`cargo fmt`). Always run `make fmt` before committing.
- Use `unwrap` only when: (1) it's provably safe (add `// SAFETY:` comment), (2) panic indicates a bug, or (3) in test code.
- Use `expect` only for unmet invariants from bad inputs or environment.
- Imports: group by `std` → external deps → `crate::` locals, separated by blank lines.
- Logging: always include a `target`, e.g. `log::info!(target: "lampo-chain", "...")`. Most logs should be at debug level.
- Use `FIXME` comments for unclear optimizations or ugly corner cases.
- Keep code simple. Don't overdesign. Write for today, not hypothetical futures.

## Git Commits

- Commit messages must be imperative, capitalized, no period: "Add support for X" not "Added support for X."
- Subject line ≤ 50 chars. Wrap body at 72 chars.
- Each commit must pass all tests, lints, and checks independently.
- **Never include fixup commits in a PR.** If a commit introduces a problem (e.g. formatting), squash the fix into the original commit. Do not leave separate "fix formatting" or "fix lint" commits in the history.
- May include a crate prefix: `cli:`, `chain:`, `node:`, `docs:`, `ci:`.

## PR Workflow

- Keep changesets small, specific, and uncontroversial.
- Isolate changes in separate commits for review, but each must be self-contained.
- Rebase on `main` when needed. Do not merge commits.
- Don't make unrelated changes unless it's an obvious improvement to code you're already touching.

## Dependencies

- Check with maintainers before adding new dependencies.
