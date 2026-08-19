## Problem

<!-- Describe the user-visible problem. Link the agreed issue when required by CONTRIBUTING.md. -->

## Solution

<!-- Explain the smallest behavior change that solves the problem. -->

## Validation

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo test --workspace --all-targets`
- [ ] `cargo check -p riddle --features install-bins --bins`
- [ ] Strict workspace Clippy passes
- [ ] User documentation is updated when behavior changed

## Scope

- [ ] The change contains no unrelated refactors or generated build artifacts
