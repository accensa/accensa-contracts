# Support

`accensa-contracts` holds the on-chain half of Accensa: `ReceiptAnchor` (Merkle batch anchoring) and `RefundVault` (policy-bounded refunds), built on `soroban-sdk` 27.0.4 and deployed on Stellar testnet.

This page is the single source of truth for how to reach the Accensa maintainers. Issues, PR templates, and documentation link here rather than embedding chat invites directly, so that if an invite link ever rotates only this file needs to change.

---

## Community channels

| Channel | Best for |
|---|---|
| [Telegram](https://t.me/+Gflo5jZStw1jMjE0) | Quick questions, claiming an issue, unblocking mid-PR |
| [Discord](https://discord.gg/5aprtMSyR) | Longer design discussion, architecture questions, async threads |

Both channels are staffed by the maintainers. If you are working on a Drips Wave issue and are blocked, use them — a question asked early costs far less than a PR built on a wrong assumption.

---

## Getting help with a contribution

**Before you ask**, the following usually answer the question faster:

- [`README.md`](README.md) — what the contracts do and how to build them
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — contribution workflow and code quality standards
- [`docs/SECURITY_MODEL.md`](docs/SECURITY_MODEL.md) — the threat model; read this before changing anything that touches funds or authorization
- [`DEPLOYMENTS.md`](DEPLOYMENTS.md) — current testnet addresses
- [`TROUBLESHOOTING.md`](TROUBLESHOOTING.md) — common build and deploy failures
- [Contract documentation](https://accensa.github.io/accensa-app/docs/contracts/overview)

**When you do ask**, include the issue number, what you have already tried, and the exact command and output if something is failing. "It doesn't work" takes several round trips to resolve; a pasted error message usually takes one.

---

## Working on a Drips Wave issue

Accensa participates in the Drips Stellar Wave. If you are contributing through a Wave:

- **Get assigned before you start.** Unassigned PRs are not guaranteed a review slot, and Wave rewards are tied to assignment.
- **Ask early if the issue is ambiguous.** The issue's `## What to build` section states which decisions are yours to make. If something outside that is unclear, ask rather than guess.
- **Your PR description must include `Closes #<issue number>`** so the issue resolves automatically on merge.
- **Run the repo's checks before opening the PR:**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test
cargo build --target wasm32v1-none --release
```

- **If your PR is merge-blocked for reasons outside your control** — a release freeze, an upstream dependency, a testnet outage — say so in the PR. Maintainers will still resolve the issue before the Wave closes so your contribution is credited.

### One rule specific to this repo

Event topics and field names are a **public interface** consumed by external indexers. Changing an event's topic tuple, adding or removing fields, or renaming a field is a **breaking change** requiring a major version bump and a public announcement — see the Event Stability Policy in [`CONTRIBUTING.md`](CONTRIBUTING.md). If your change touches events, raise it before you build.

---

## Reporting a bug

Open an issue in this repository with reproduction steps, the expected and actual behavior, and your environment. If the bug has security implications, **do not open a public issue** — follow [`SECURITY.md`](SECURITY.md) instead.

## Reporting a vulnerability

See [`SECURITY.md`](SECURITY.md). Please report privately; do not open a public issue or discuss it in the community channels.

---

## What this page does not cover

Maintainers cannot provide production support, integration consulting, or debugging of your own application code. The channels above are for contributing to this repository and for questions about how Accensa itself behaves.
