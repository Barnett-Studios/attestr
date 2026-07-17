# Contributing to attestr

Thanks for your interest in attestr — Promise-Theory verification: findings plus a per-agent trust delta. It is one component of the
[Barnett Studios agentic-harness toolkit](https://github.com/Barnett-Studios). This guide
covers setup, the quality bar every change has to clear, and the conventions that keep the
project consistent.

By contributing you agree that your contributions are dual-licensed under the project's
[MIT](LICENSE-MIT) and [Apache-2.0](LICENSE-APACHE) licenses (inbound = outbound).
Participation is governed by our [Code of Conduct](CODE_OF_CONDUCT.md).

## Ways to contribute

- **Report a bug** — use the [bug report](.github/ISSUE_TEMPLATE/bug_report.yml) form.
- **Request a feature** — use the [feature request](.github/ISSUE_TEMPLATE/feature_request.yml) form.
- **Improve docs** — the README, the CONTRACT, or this guide.
- **Fix a bug or land a feature** — open an issue first for anything non-trivial so the
  approach can be agreed before you invest the time.

## Development setup

```bash
rustup toolchain install stable
rustup component add rustfmt clippy

git clone https://github.com/Barnett-Studios/attestr
cd attestr
cargo build
```

The toolchain is pinned in `rust-toolchain.toml`; plain `rustup` + `cargo` is fully supported.

## The quality bar

Every PR has to pass the same gates CI enforces. Run them locally before pushing:

| Gate | Command | Requirement |
|---|---|---|
| Format | `cargo fmt --all -- --check` | clean |
| Lint | `cargo clippy --all-targets -- -D warnings` | zero warnings |
| Tests | `cargo test --locked` | all pass |
| MSRV | build on the pinned toolchain | compiles |

Tests are authored **together with** the code they cover — a feature PR without tests, and a
bug fix without a test that fails before the fix and passes after, won't pass review.

## Commit conventions

We use [Conventional Commits](https://www.conventionalcommits.org/) with an optional scope,
imperative subject:

```
feat(scope): add the thing
fix(scope): stop doing the wrong thing
docs: correct the README
test: RED — the failing case first
```

Common types: `feat`, `fix`, `docs`, `test`, `refactor`, `perf`, `deps`, `chore`.
Keep commits logically scoped — the project preserves per-commit history on merge, so a clean
series reviews far better than one giant commit.

## The contract is the product

attestr is defined by its [`CONTRACT.md`](CONTRACT.md): the interface, the invariants, and the
guarantees other components rely on. A change that alters observable behavior updates the
contract **in the same PR**. Swappability is the point — don't break the socket.

## Pull requests

1. Branch off `main` (fetch first — `main` moves).
2. Keep the PR focused; a smaller diff reviews faster and lands sooner.
3. Make the quality gates above green locally.
4. Fill in the [PR template](.github/PULL_REQUEST_TEMPLATE.md) — the checklist mirrors CI.
5. Update any documentation your change touches (README, CONTRACT) in the **same** PR.
6. Open the PR against `main`; CI runs the full gate set.

## Reporting a security issue

Please **don't** open a public issue for a security vulnerability. See
[SECURITY.md](SECURITY.md) — report it privately via GitHub's
[security advisory](https://github.com/Barnett-Studios/attestr/security/advisories/new) form so it can be
fixed before disclosure.

---

Questions that don't fit an issue? Open a
[discussion](https://github.com/Barnett-Studios/attestr/discussions). Thanks for contributing.
