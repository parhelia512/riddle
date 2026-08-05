# Contributing to Riddle

Riddle is an experimental language. Public syntax, APIs, and ABI details may
change, so agreeing on the problem and scope before implementation saves work
for both contributors and reviewers.

## Choose Work

Small bug fixes with a clear regression test may be submitted directly. Open an
issue and wait for maintainer agreement before implementing:

- new language or standard-library features;
- changes to public syntax, type-system behavior, or diagnostics;
- C ABI, FFI, runtime, or garbage-collector changes;
- async, concurrency, I/O, or error-model design;
- changes spanning several compiler stages or tools.

An issue labeled `good first issue` or `help wanted` is ready only when it has
concrete acceptance criteria. A missing API or an item mentioned as a current
limitation is not automatically accepted work.

Documentation-only fixes, typo fixes, and narrowly scoped test improvements do
not need a planning issue.

## Make Changes

- Branch from the latest `main`.
- Keep each pull request focused on one problem.
- Add the smallest regression test that demonstrates the changed behavior.
- Update user documentation when public behavior changes.
- Do not include unrelated refactors or generated build artifacts.

## Validate

Run the same checks used by pull-request CI:

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo check -p riddle --features install-bins --bins
cargo clippy --workspace --all-targets --all-features --keep-going -- -D warnings -D clippy::pedantic -D clippy::nursery -D clippy::cargo -A clippy::multiple_crate_versions
```

## Open a Pull Request

Describe the problem, the chosen solution, and the validation performed. Link
the agreed issue when one is required. Open a draft pull request when early
feedback would prevent wasted implementation work.

All pull-request checks must pass before merge. A passing check confirms the
current automated gates; it does not replace design review.
