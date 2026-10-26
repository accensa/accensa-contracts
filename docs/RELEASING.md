# Releasing

This document outlines the process for cutting, tagging, and deploying a new version of the Accensa smart contracts (`ReceiptAnchor` and `RefundVault`).

## Versioning Policy

We use Semantic Versioning (SemVer) with the following contract-specific definitions:

- **MAJOR (`x.0.0`)**: Breaking changes. This includes changes to the exported function signatures (names, arguments, return types), removal of functions, changes to the `DataKey` layout that require migration, or breaking changes to the event topics and data maps (as per the Event Stability Policy).
- **MINOR (`0.x.0`)**: Backwards-compatible additions. This includes adding new functions, new events, or adding new `DataKey` fields that do not break existing state.
- **PATCH (`0.0.x`)**: Backwards-compatible bug fixes or optimizations (e.g., reducing CPU/memory footprint) that do not alter the public interface or event shapes.

**Pre-1.0 exception (the rule this repository follows).** While the project is
below `1.0.0`, a **breaking** change bumps the **minor** version (`0.x.0`)
instead of the major one, and the changelog entry marks it as breaking. This is
the single authoritative statement of the pre-1.0 rule: `0.2.0` shipped the
event-topic changes called out under [`CHANGELOG.md`](../CHANGELOG.md)'s
`⚠️ Breaking` heading, and `0.3.0` shipped further breaking changes — both as
minor releases. From `1.0.0` onward the strict MAJOR/MINOR/PATCH definitions
above apply with no exception.

## Release Process

When preparing a release, follow these steps. They guarantee provenance — every deployed WASM traces back to its exact source commit — but **not** byte-identical builds across machines unless the same compiler is used; step 3 states exactly what is and is not reproducible, and the toolchain involved:

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

### 3. Build the WASM and Record the Toolchain
Build the WebAssembly artifacts using the optimized release profile:
```bash
cargo build --target wasm32v1-none --release
```
Our `build.rs` scripts embed the current `GIT_SHA` into the compiled WASM, so any deployed contract can be traced back to its exact source code commit. That source-to-commit provenance — commit → tag → artifact — is what this release process guarantees.

Byte-identical WASM is **not** guaranteed by the repository as it stands: no Rust toolchain is pinned (there is no `rust-toolchain.toml` and no `.cargo/config.toml` in the tree), and CI installs `stellar-cli` with `cargo install --locked` rather than recording a compiler version (`.github/workflows/ci.yml`). Different `rustc` versions emit different WASM bytes for the same commit, so the same source built on two toolchains yields two hashes. The release toolchain is Rust `stable` with the `wasm32v1-none` target — the same channel CI builds with. To make a released artifact reproducible, record the exact compiler when cutting the release and build with it:
```bash
rustc --version   # record alongside the tag (tag message or DEPLOYMENTS.md)
cargo --version
```
With the source tag and the recorded `rustc`/`cargo` versions as inputs, a reader can rebuild the released artifact from source; without them, only the commit provenance is reproducible.

"Optimized" here means the cargo profile (`opt-level = "z"`, LTO, single codegen
unit), not the deployable binary. `./deploy.sh` runs `stellar contract build`,
which additionally passes the wasm through the optimizer, and that step shrinks
every contract by roughly 25% — a size that matters, because a contract code
ledger entry is capped at 131072 bytes. To measure the artifact that actually
gets deployed (this is what CI does before the size gate):
```bash
cargo build --locked --workspace --exclude testutils --target wasm32v1-none --release
bash scripts/optimize_wasm.sh
```

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
