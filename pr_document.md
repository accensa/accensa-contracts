## Summary
Integrates a lightweight Groth16 zero-knowledge validity proof verifier tailored for Soroban to enable O(1) batch validity verification on `ReceiptAnchor`.

Closes #127

## What Changed
- **ZK Verifier Module (`zk_verifier`):** Implemented `ZkProof`, `VerifyingKey`, `verify_groth16`, and `verify_batch_zk_proof` in `contracts/receipt-anchor/src/zk_verifier.rs`.
- **`ReceiptAnchor` Contract Entry Points:** Added `anchor_batch_zk(state_root, proof, count, period_start, period_end) -> u64` and `verify_zk_proof(proof, vk, public_inputs) -> bool`.
- **Canonical Error Enum:** Added `Error::InvalidProof = 203` to `contracts/common/src/lib.rs`.
- **Documentation:** Updated `README.md` with new ZK functions and error codes.
- **Testing:** Added end-to-end verification and rejection unit tests in `contracts/receipt-anchor/src/test.rs`.

## Acceptance Criteria Checklist
- [x] ZK verifier logic is implemented in the contract (`zk_verifier.rs`, `verify_zk_proof`).
- [x] The anchoring transaction accepts a ZK proof instead of a Merkle root (`anchor_batch_zk`).
- [x] Tests demonstrate end-to-end verification of a valid proof and rejection of invalid ones (`test_anchor_batch_zk_valid_proof_succeeds`, `test_anchor_batch_zk_invalid_proof_rejected`, `test_verify_zk_proof_end_to_end`).

## Test Results
- `cargo test`: All workspace unit tests, property fuzz tests, and cross-contract integration tests passing (100% pass rate).
- `cargo clippy --all-targets -- -D warnings`: 0 warnings, clean.
- `cargo fmt --check`: Clean formatting across the entire workspace.

## Security Note
Validity proofs are verified with strict bounds, structural format checks, and point validations before state roots are recorded in persistent storage.
