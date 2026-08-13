# Storage Audit

This document audits the storage calls made by the `accensa-contracts` to determine and justify the storage class (Instance, Persistent, Temporary) for each entry.

## Storage Entries

### `ReceiptAnchor`

1. `DataKey::Admin`
   - **Class**: Instance
   - **Contents**: The `Address` of the merchant who controls the contract.
   - **Size**: Small (32 bytes).
   - **Justification**: The admin address is required for every authenticated operation. It is core contract state that affects the entire instance and should be loaded on every contract invocation. If lost, the contract cannot be managed. It belongs in Instance storage.
   - **TTL Strategy**: Managed by the instance TTL (bumped on contract interactions).

2. `DataKey::BatchCount`
   - **Class**: Instance
   - **Contents**: A `u64` tracking the total number of batches anchored.
   - **Size**: Small (8 bytes).
   - **Justification**: This is an incrementing global counter used as the key for new batches. If lost, the contract cannot safely anchor new batches without colliding IDs. It must be present for anchoring and should not be archived independently. Belongs in Instance storage.
   - **TTL Strategy**: Managed by the instance TTL.

3. `DataKey::PrunedUpTo`
   - **Class**: Instance
   - **Contents**: A `u64` tracking the ID of the last batch that was pruned.
   - **Size**: Small (8 bytes).
   - **Justification**: Acts as a global cursor for the pruning operation to ensure we don't scan from 1 every time. Core instance state.
   - **TTL Strategy**: Managed by the instance TTL.

4. `DataKey::Batch(u64)`
   - **Class**: Persistent
   - **Contents**: A `BatchRecord` struct containing the Merkle root, count, period, and anchored ledger.
   - **Size**: Moderate (approx 56 bytes).
   - **Justification**: This is an audit trail. An agent needs to be able to verify a receipt against this batch long after the payment occurs. It cannot be Temporary because it must not be arbitrarily deleted by the network when rent expires—if a batch is missing, the agent has no proof they paid. It cannot be Instance because there are unbounded amounts of batches and Instance storage is loaded on every contract call (limited size). It must be Persistent.
   - **TTL Strategy**: `TTL_EXTEND` is 518,400 ledgers (~30 days) with a `TTL_THRESHOLD` of 100. This provides a 30-day window for agents to verify their receipts before the batch requires restoration, balancing rent costs against the verification window.

### `RefundVault`

1. `DataKey::Admin`
   - **Class**: Instance
   - **Contents**: The `Address` of the merchant.
   - **Size**: Small (32 bytes).
   - **Justification**: Used for all admin operations (deposit, withdraw, refund). Belongs in Instance storage.
   - **TTL Strategy**: Managed by the instance TTL.

2. `DataKey::Token`
   - **Class**: Instance
   - **Contents**: The `Address` of the SAC token (e.g. USDC).
   - **Size**: Small (32 bytes).
   - **Justification**: Needed for all token transfers. Global configuration. Belongs in Instance storage.
   - **TTL Strategy**: Managed by the instance TTL.

3. `DataKey::RefundWindow`
   - **Class**: Instance
   - **Contents**: A `u32` representing the number of ledgers after a payment during which a refund is valid.
   - **Size**: Small (4 bytes).
   - **Justification**: Policy parameter that applies to the entire vault. Belongs in Instance storage.
   - **TTL Strategy**: Managed by the instance TTL.

4. `DataKey::IsPaused`
   - **Class**: Instance
   - **Contents**: A `bool` flag for the emergency stop.
   - **Size**: Small (1 byte).
   - **Justification**: Affects every state-changing operation globally. Belongs in Instance storage.
   - **TTL Strategy**: Managed by the instance TTL.

5. `DataKey::Refund(BytesN<32>)`
   - **Class**: Persistent
   - **Contents**: A `RefundRecord` containing the refunded amount, recipient, and ledger.
   - **Size**: Moderate (approx 52 bytes).
   - **Justification**: This prevents double-refunds. If it were Temporary and the network deleted it, the merchant could be subjected to a double refund exploit for the same payment reference. It must be Persistent to guarantee the "already refunded" invariant. It cannot be Instance because the number of refunds is unbounded.
   - **TTL Strategy**: `TTL_EXTEND` is 518,400 ledgers (~30 days) with a `TTL_THRESHOLD` of 100. This ensures the double-refund protection remains active for the duration of the typical refund window without requiring restoration.

6. Unused Variants (`Metadata`, `RefundMax`, `Admins`, `Threshold`)
   - **Note**: Several variants are defined in the `DataKey` enum but are not actively used in storage calls.

## Rent Cost Implications

Persistent storage on Soroban incurs rent. A merchant pays to keep $N$ batches and $M$ refund records alive.
Since each `BatchRecord` and `RefundRecord` is small (under 100 bytes), the storage cost per entry is minimal.
However, because they accumulate indefinitely, the aggregate rent can grow. The merchant manages this via the `prune_batches` function in `ReceiptAnchor`, which explicitly deletes `BatchRecord`s older than a specified ledger, reclaiming the rent deposits for those entries.
Refunds are inherently bounded by the `RefundWindow`, and old refund records can eventually be archived by the network after the 30-day TTL expires, meaning the merchant ceases paying active rent on them while they remain restorable if ever needed.
For full quantitative fee and rent analysis, refer to [MAINNET_DEPLOYMENT.md](MAINNET_DEPLOYMENT.md).

## Archival and Restore

When a `Persistent` entry like `BatchRecord` or `RefundRecord` reaches its TTL without being extended, it is archived by the network.
- **Archival**: The data is removed from the active state, saving rent. It cannot be read by smart contracts.
- **Restore**: An archived entry is not lost. It can be brought back to the active state by submitting a `RestoreFootprintOp` transaction containing the key. Once restored, the entry receives a new TTL and is readable again.
This mechanism is why it is safe for old records to expire: they are not permanently destroyed, but they stop costing the merchant ongoing rent.

## Conclusion

All state currently stored as `Persistent` (`BatchRecord` and `RefundRecord`) serves as critical audit trails and double-spend protection. They cannot be reconstructed safely without compromising the security model of the contracts, and they cannot be `Temporary` because their deletion would allow double refunds or prevent receipt verification. Global configurations and counters correctly reside in `Instance` storage. No state can or should be moved to `Temporary`.
