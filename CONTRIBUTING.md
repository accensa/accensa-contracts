# Contributing to Accensa Contracts

We welcome contributions from the community! Whether it's a bug fix, new feature, or documentation improvement for our Soroban smart contracts, your help is appreciated.

## Getting Started

1. **Fork the repository** on GitHub.
2. **Clone your fork** locally.
3. **Find an issue**: Look for issues labeled with `good first issue` if you are a new contributor. If you have an idea for a feature or found a bug, please create a new issue first to discuss it with the maintainers before writing code.
4. **Wait for assignment**: To avoid duplicate work, please express your interest on the issue and wait for a maintainer to assign it to you before starting work.
5. **Create a new branch** for your feature or bug fix (`git checkout -b feature/my-new-feature` or `bugfix/issue-123`).
6. **Make your changes** and test them thoroughly.
7. **Keep `Cargo.lock` committed**: CI uses `--locked`, so dependency changes must update and commit the lockfile in the same change. Do not rely on CI to resolve dependencies implicitly.

### Ignoring Mechanical Formatting Revisions in Git Blame

This repository contains a `.git-blame-ignore-revs` file to filter out mechanical formatting and lint sweeps when inspecting line history with `git blame`.

To enable it locally for your clone, run:

```bash
git config blame.ignoreRevsFile .git-blame-ignore-revs
```


## Submitting a Pull Request

- Ensure your code follows the existing Rust style conventions.
- Run all local build and test commands (e.g., `cargo build --target wasm32v1-none --release`, `cargo test`) before submitting.
- Provide a clear and descriptive PR title and description.
- Link to any relevant open issues in your PR description (e.g. `Closes #123`).
- **Changelog Enforcement**: Any PR modifying contract source code (`contracts/**/src/**`) must include an entry in [`CHANGELOG.md`](CHANGELOG.md) under `## [Unreleased]`.
  - **Escape Hatch**: If a PR contains genuinely internal or non-functional changes (e.g. refactoring, comments, or internal tests) that do not warrant a user-facing changelog entry, attach the `skip-changelog` label to your pull request to bypass the CI check.
- Wait for a maintainer to review your PR. Address any feedback as needed.

## Reporting Bugs and Requesting Features

If you find a bug or have a feature idea, please open an issue on GitHub using our issue templates.
Include as much detail as possible to help us understand and resolve the issue quickly.

## Event Stability Policy

Event topics and field names are a **public interface** consumed by external indexers. Changing an event's topic tuple, adding/removing fields, or changing field names is considered a **breaking change** and requires a major version bump and a public announcement.

When writing an indexer against these contracts, you should:
- Subscribe specifically by the topics documented in [`docs/EVENTS.md`](docs/EVENTS.md).
- Tolerate unknown fields in the event data map to allow for non-breaking additions in the future.

## Resource Cost Baselines & Re-baselining

This repository enforces contract resource cost regression gates in unit and fuzz tests (e.g. `contracts/receipt-anchor/src/fuzz_test.rs` and `contracts/refund-vault/src/fuzz_test.rs`). These tests assert that host CPU instruction costs and memory byte allocations for critical operations (`anchor_batch`, `verify_receipt`, and `refund`) remain within a 15% headroom of documented baselines.

If a legitimate contract modification or optimization increases resource usage and triggers a cost regression failure in CI:

1. **Verify Necessity**: Confirm that the cost increase is expected and cannot be mitigated through code optimization.
2. **Measure New Baseline**: Run the cost regression tests locally with unoptimized debug builds:
   ```bash
   cargo test --package receipt-anchor benchmark_gas_and_cpu_instructions
   cargo test --package refund-vault test_refund_resource_cost_budget
   ```
3. **Update Constants**: Update the baseline constants (`ANCHOR_BATCH_BASELINE_CPU`, `ANCHOR_BATCH_BASELINE_MEM`, `VERIFY_RECEIPT_BASELINE_CPU`, `VERIFY_RECEIPT_BASELINE_MEM`, `REFUND_BASELINE_CPU`, `REFUND_BASELINE_MEM`) in the respective `fuzz_test.rs` files to reflect the new measured values.
4. **Document in PR**: In your pull request description, explicitly note the baseline update, state the measured values before and after, and explain why the increased resource consumption is necessary.

Thank you for helping make Accensa better!
