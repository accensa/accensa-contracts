- closes #398
- closes #380
- closes #294
- closes #290

### Changes Made

- **Issue #398**: Added Standardized SEP-0026 Soroban Contract Registry Metadata in the contract `lib.rs` files using `rsrvmeta`.
- **Issue #380**: Standardized the `Error` code enum in `contracts/common/src/lib.rs` by adding a specific `HostError` variant to explicitly map Soroban host errors.
- **Issue #294**: Improved inline documentation and comments in `contracts/refund-vault/src/fuzz_test.rs` to better explain the property test invariants.
- **Issue #290**: Improved inline documentation and comments in `contracts/receipt-anchor/src/fuzz_test.rs` to better explain the operations and invariant checking logic.
