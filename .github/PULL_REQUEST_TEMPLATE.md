<!--
Thanks for contributing to attestr! Keep the PR focused and fill in the sections
below. The checklist mirrors the CI gates — a box you can't tick is a signal to
fix before requesting review, not to hide.
-->

## What & why

<!-- What does this change do, and what problem does it solve? Link the issue. -->

Closes #

## How it was verified

<!-- The commands you ran and what you observed. "Trust me" isn't verification. -->

## Checklist

- [ ] `cargo fmt --all -- --check` is clean
- [ ] `cargo clippy --all-targets -- -D warnings` has zero warnings
- [ ] `cargo test --locked` passes, and new/changed code is covered
- [ ] Docs updated in this PR if a documented surface changed (README, CONTRACT)
- [ ] Commits follow Conventional Commits and are logically scoped

## Notes for reviewers

<!-- Trade-offs, follow-ups, anything you're unsure about. Optional. -->
