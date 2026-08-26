# Releasing

This document outlines the process for cutting, tagging, and deploying a new version of the Accensa smart contracts (`ReceiptAnchor` and `RefundVault`).

## Versioning Policy

We use Semantic Versioning (SemVer) with the following contract-specific definitions:

- **MAJOR (`x.0.0`)**: Breaking changes. This includes changes to the exported function signatures (names, arguments, return types), removal of functions, changes to the `DataKey` layout that require migration, or breaking changes to the event topics and data maps (as per the Event Stability Policy).
- **MINOR (`0.x.0`)**: Backwards-compatible additions. This includes adding new functions, new events, or adding new `DataKey` fields that do not break existing state.
- **PATCH (`0.0.x`)**: Backwards-compatible bug fixes or optimizations (e.g., reducing CPU/memory footprint) that do not alter the public interface or event shapes.

## Release Process

When preparing a release, follow these steps to ensure reproducible builds and provenance:

### 1. Update Versions and Changelog
Update the `version` field in both `Cargo.toml` files:
- `contracts/receipt-anchor/Cargo.toml`
- `contracts/refund-vault/Cargo.toml`

Update [`CHANGELOG.md`](../CHANGELOG.md):
- Rename the `## [Unreleased]` heading to the new version and release date, e.g., `## [1.0.0] — YYYY-MM-DD`.
- Add a fresh `## [Unreleased]` section above it for future changes.

### 2. Cut a Release Branch and Tag
Commit the version bump and create a Git tag for the release.
```bash
git add .
git commit -m "chore: release v1.0.0"
git tag -a v1.0.0 -m "Release v1.0.0"
git push origin main --tags
```

### 3. Build Reproducibly
Build the WebAssembly artifacts using the optimized release profile:
```bash
cargo build --target wasm32v1-none --release
```
Our `build.rs` scripts automatically embed the current `GIT_SHA` into the compiled WASM, ensuring that any deployed contract can be traced back to its exact source code commit.

### 4. Deploy and Record
Deploy the contracts using the deployment script. The script automatically computes the WASM hashes and extracts the version and commit SHA.
```bash
./deploy.sh
```
This produces `deployments/<network>.env` containing the `NEXT_PUBLIC_RECEIPT_ANCHOR_ID`, `NEXT_PUBLIC_REFUND_VAULT_ID`, version numbers, commit SHA, and WASM hashes.

### 5. Update `DEPLOYMENTS.md`
Manually copy the recorded hashes, versions, and commit SHA from the `.env` file into `DEPLOYMENTS.md` for human readability.
Commit these documentation updates to the repository.
```bash
git add DEPLOYMENTS.md deployments/
git commit -m "docs: record v1.0.0 deployment"
git push origin main
```
