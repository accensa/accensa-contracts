# Contributing to Accensa Contracts

We welcome contributions from the community! Whether it's a bug fix, new feature, or documentation improvement for our Soroban smart contracts, your help is appreciated.

## Getting Started

1. **Fork the repository** on GitHub.
2. **Clone your fork** locally.
3. **Find an issue**: Look for issues labeled with `good first issue` if you are a new contributor. If you have an idea for a feature or found a bug, please create a new issue first to discuss it with the maintainers before writing code.
4. **Wait for assignment**: To avoid duplicate work, please express your interest on the issue and wait for a maintainer to assign it to you before starting work.
5. **Create a new branch** for your feature or bug fix (`git checkout -b feature/my-new-feature` or `bugfix/issue-123`).
6. **Make your changes** and test them thoroughly.

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
- Wait for a maintainer to review your PR. Address any feedback as needed.

## Reporting Bugs and Requesting Features

If you find a bug or have a feature idea, please open an issue on GitHub using our issue templates.
Include as much detail as possible to help us understand and resolve the issue quickly.

## Event Stability Policy

Event topics and field names are a **public interface** consumed by external indexers. Changing an event's topic tuple, adding/removing fields, or changing field names is considered a **breaking change** and requires a major version bump and a public announcement.

When writing an indexer against these contracts, you should:
- Subscribe specifically by the topics documented in [`docs/EVENTS.md`](docs/EVENTS.md).
- Tolerate unknown fields in the event data map to allow for non-breaking additions in the future.

Thank you for helping make Accensa better!
