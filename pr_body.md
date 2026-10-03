- closes #375
- closes #385
- closes #386
- closes #388

### Changes Implemented
- **Issue #375**: Implemented `extract_fee` with a Tiered Fee Assessment Hook in `refund-vault-factory/src/fee.rs`, varying the fee assessed depending on the tier.
- **Issue #385**: Added a Batch Transaction Execution Pipeline by implementing `execute_batch` on `MultisigAccount` to run sequences of calls within a single transaction in `multisig-account/src/lib.rs`.
- **Issue #386**: Implemented `verify_zk_commitment` on `StateChannel` for Zero-Knowledge Commitment Verification of Off-Chain Settlements in `state-channel/src/lib.rs`.
- **Issue #388**: Implemented `ReentrancyGuard` module in `common/src/reentrancy.rs` to serve as a Cross-Contract Call Reentrancy Guard Protocol across applications.
