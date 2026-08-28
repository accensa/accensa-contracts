# PR: Migrate to Advanced Memory Management for WASM Blob Processing

This PR optimizes Merkle proof verification inside the `ReceiptShard` contract to use a guest-side static stack buffer and a pure Wasm SHA-256 implementation. This eliminates heap allocations and host roundtrips for intermediate hashing during proof processing, ensuring that memory usage remains flat and stable regardless of Merkle tree depth or batch size.

## Summary of Changes
1. **Added `sha2` Dependency**: Added the `sha2` crate (version `0.10.9`) with `default-features = false` to `receipt-shard/Cargo.toml` to enable standard-library-free stack-based hashing in the WASM guest.
2. **Static Buffer Merkle Proof Copying**: In `ReceiptShard::verify_receipt`, we copy the Merkle proof `BytesN<32>` elements from the host vector into a stack-allocated array `[[u8; 32]; 128]` before running the verification loop. This decouples the host-roundtrip loading phase from the actual computation loop.
3. **Pure Wasm Hashing**: Refactored the core Merkle hashing loop in `verify_receipt` to use the stack-based `Sha256` from the `sha2` crate. This avoids crossing the host-guest boundary with `env.crypto().sha256()` and eliminates the creation of temporary `soroban_sdk::Bytes` host objects on every loop iteration.
4. **Testutils Deprecation Fix**: Updated deprecated calls to `env.budget()` in `contracts/testutils/src/budget.rs` to use `env.cost_estimate().budget()`.
5. **Memory Scaling Benchmark**: Added a dedicated benchmark test `test_verify_receipt_memory_scaling_benchmark` inside `contracts/receipt-anchor/src/test.rs` to measure and verify resource scaling across various proof lengths (8, 16, 32, 64).
6. **Snapshot Updates**: Re-generated test snapshots to reflect updated CPU instruction counts from guest-side hashing.

---

## Key Design Decisions & Memory Profile
* **Static Stack Buffers**: Merkle proofs for batch sizes under `MAX_BATCH_SIZE` (1000) have a maximum depth of `10`. To support scaling to much larger batches in the future, we use a static array of size `128` (4 KB total on the stack), which easily handles proofs for up to $2^{128}$ leaves. This guarantees zero dynamic guest-side heap allocations.
* **Host vs Guest CPU Tradeoff**: Pure WASM hashing avoids host allocations but executes more WASM guest instructions. Since host memory limits are a critical constraint under high Merkle proof scaling, moving hashing to pure WASM is the optimal choice for ensuring reliable, OOM-free transactions under peak loads.

### Scaling Profile (WASM Guest Memory Footprint)
As measured in `test_verify_receipt_memory_scaling_benchmark`:
* **Proof length 8**: Mem footprint delta = `0 bytes`
* **Proof length 16**: Mem footprint delta = `0 bytes`
* **Proof length 32**: Mem footprint delta = `0 bytes`
* **Proof length 64**: Mem footprint delta = `0 bytes`

Memory footprint remains **flat (0 bytes delta)** across all batch sizes and proof depths, preventing any OOM errors.

---

## Contract Change Safety Checklist
Please verify that your changes adhere to contract stability requirements:

- [x] **Event Shapes**: Does this PR modify event topic tuples or data shapes? No events are added or modified (fully adheres to `docs/EVENTS.md`).
- [x] **Storage Layout**: Does this PR change storage keys or layout? No changes to storage layout or keys.
- [x] **Error Variants**: Does this PR add or renumber contract error codes? No changes to error variant definitions.
- [x] **Changelog**: Has a corresponding entry been added to `CHANGELOG.md`? Yes, added under `### Changed` in the `[Unreleased]` section.
- [x] **Deployments**: Has any impact on deployed contracts or `DEPLOYMENTS.md` been documented? None, purely internal optimization of `verify_receipt` without contract interface or signature changes.
- [x] **Verification**: Has this change been tested locally (`cargo test`) and/or exercised on Soroban testnet? Yes, successfully ran and passed the entire test suite locally (115 tests) including a new memory scaling benchmark.

---

## Acceptance Criteria Checklist
* [x] Arena allocator or static buffer strategy is implemented (`proof_buffer: [[u8; 32]; 128]` on the stack).
* [x] WASM memory footprint is flat and stable during proof verification.
* [x] Benchmarks prove the ability to process larger data blobs without out-of-memory errors (differential guest memory usage is `0 bytes` for all test depths).

---

## Security & Integrity Note
All existing security validations, including sorted-pair SHA-256 conventions and authorization boundaries, are fully preserved. Pure Wasm hashing does not modify the Merkle root definition or event stability schemas.
