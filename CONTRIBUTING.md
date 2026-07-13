# Contributing to Accensa Contracts

Thank you for your interest in contributing to Accensa! We are part of the Drips Wave program on Stellar.

## Getting Started

1.  Look for open issues tagged with \`complexity: 100\`, \`complexity: 150\`, or \`complexity: 200\`.
2.  If an issue is unassigned, leave a comment asking to work on it.
3.  Wait for the maintainer to assign it to you.
4.  Fork the repo and create a branch for your feature: \`git checkout -b feat/your-feature-name\`.

## Pull Request Process

1.  Ensure all code runs successfully and passes CI.
2.  Write clear, conventional commit messages (e.g. \`feat(contracts): add refund window parameter\`).
3.  In your PR description, link the issue it resolves using \`Closes #123\`.
4.  A maintainer will review your code. Once approved and merged, you earn points for the Wave based on the issue complexity!

## Code Quality Standards
- For Rust/Soroban: We enforce no \`unwrap()\` outside of tests, no floats, basis-points math, and the use of the \`soroban-sdk\` idioms. All functions must be fully tested.
