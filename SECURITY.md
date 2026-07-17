# Security Policy

## Reporting a vulnerability

**Please do not report security vulnerabilities through public GitHub issues,
discussions, or pull requests.**

Report them privately through GitHub's
[security advisory form](https://github.com/Barnett-Studios/attestr/security/advisories/new).
This lets us investigate and ship a fix before the issue is public.

Please include:

- a description of the vulnerability and its impact,
- the `attestr` version and how it was built or installed,
- steps to reproduce, ideally a minimal proof of concept.

We aim to acknowledge a report within a few days and will keep you updated as we
work on a fix. We follow coordinated disclosure: once a fix is available we'll
publish an advisory and credit you, unless you'd prefer to remain anonymous.

## Supported versions

Security fixes land on the **latest published release line** and ship in a new
patch release. We don't backport to older minor or major versions — upgrade to
the latest release to stay covered.

| Version | Supported |
|---|---|
| latest release | :white_check_mark: |
| older releases | :x: (upgrade to latest) |

## Scope

`attestr` is a component of the [Barnett Studios agentic-harness
toolkit](https://github.com/Barnett-Studios). It runs locally as part of a
developer/CI workflow. Reports about the component mishandling untrusted input
(a crafted repository, manifest, diff, or config it is pointed at) are in scope.
