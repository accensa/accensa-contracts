# Storage Audit

This document details the storage architecture, data classifications, and rent cost implications for the Accensa contracts (`ReceiptAnchor` and `RefundVault`).

## Storage Enumeration and Justifications

Soroban provides three storage classes:
- **Instance**: Bound to the contract instance, loads automatically, and archived as a unit.
- **Persistent**: Key-value entries that survive independently, requires rent, and can be restored if archived.
- **Temporary**: Automatically deleted after TTL expiration, cannot be restored.

### `ReceiptAnchor`

| DataKey | Class | Contents | Size | Justification |
|---|---|---|---|---|
| `Admin` | Instance | `Address` (Merchant) | Small | Required for authentication of merchant operations (`anchor_batch`, `prune_batches`). Essential state that must always be available. |
| `BatchCount` | Instance | `u64` (Sequence) | Small | Tracks the latest batch ID to ensure monotonic assignment. Cannot be reconstructed on-chain efficiently without full event replay. |
| `PrunedUpTo` | Instance | `u64` (Cursor) | Small | Maintains the lower-bound of active batches. Essential for efficient pruning iterations. |
| `Batch(u64)` | Persistent | `BatchRecord` | ~100 bytes | Holds the Merkle root, count, period, and ledger. Required for on-chain `verify_receipt` execution. While `AnchorEvent` emits this data, on-chain functions cannot read events. Must be persistent to prevent arbitrary deletion; if archived, it can be restored to prove old receipts. |

### `RefundVault`

| DataKey | Class | Contents | Size | Justification |
|---|---|---|---|---|
| `Admin` | Instance | `Address` (Merchant) | Small | Required for authentication of merchant operations (`deposit`, `refund`, `withdraw`, `pause`). |
| `Token` | Instance | `Address` (USDC SAC) | Small | The underlying asset contract address. Crucial for token transfers. |
| `RefundWindow` | Instance | `u32` (Ledgers) | Small | Global policy parameter determining refund eligibility. |
| `IsPaused` | Instance | `bool` | Small | Emergency halt flag. Must be immediately available at all times. |
| `Metadata` | Instance | Reserved | Variable | Reserved for future contract configuration or metadata. |
| `RefundMax` | Instance | `i128` | Small | Reserved configuration for maximum allowed refund limits. |
| `Admins` | Instance | Reserved | Variable | Reserved for potential multi-admin expansion. |
| `Threshold` | Instance | Reserved | Small | Reserved for potential multi-sig or quorum thresholds. |
| `Refund(BytesN<32>)`| Persistent | `RefundRecord` | ~100 bytes | Tracks executed refunds (amount, recipient, ledger). Critical to prevent replay attacks (double-refunding the same payment). If this were Temporary, it could expire and allow a second refund. If archived, it remains a tombstone that prevents re-creation until restored. |

*Note: The `Metadata`, `RefundMax`, `Admins`, and `Threshold` keys are defined in the `DataKey` enum for future compatibility and expansion, though some may currently be inactive in the logic.*

## TTL Strategy

Stellar uses a Time-To-Live (TTL) mechanism to manage state bloat.

- **`TTL_EXTEND`**: `518,400` ledgers (approximately 30 days, assuming ~5 seconds per ledger).
- **`TTL_THRESHOLD`**: `100` ledgers.

**Rationale**:
A 30-day `TTL_EXTEND` ensures that actively used batches and recent refund records remain in the live state without requiring manual restoration by downstream clients. The `TTL_THRESHOLD` of 100 ledgers acts as a buffer to prevent rent-bumping transactions from spamming the network on every single contract call—only extending the TTL if it drops below this threshold.

Both `Instance` storage (which covers `Admin`, `IsPaused`, etc.) and the actively modified `Persistent` entries (`RefundRecord`, `BatchRecord`) receive TTL extensions during mutations to keep the active working set alive.

## Rent Cost Implications

Persistent storage incurs rent to stay active on the Stellar network, priced at roughly **0.5 XLM per KB per year**.

- **BatchRecord**: A single record is roughly 100 bytes (root: 32b, count: 4b, periods: 16b, overhead: ~50b).
- **RefundRecord**: A single record is roughly 100 bytes (address: 32b, amount: 16b, ledger: 4b, overhead: ~50b).

**Projection**:
If a merchant processes 10,000 payments daily, batched into chunks of 500:
- 20 `BatchRecord`s per day = 7,300 batches per year.
- Storage footprint: 7,300 * 100 bytes ≈ 730 KB.
- **Rent cost for Batches**: ~365 XLM per year.

If 1% of those 10,000 daily payments require refunds:
- 100 `RefundRecord`s per day = 36,500 refunds per year.
- Storage footprint: 36,500 * 100 bytes ≈ 3.65 MB.
- **Rent cost for Refunds**: ~1,825 XLM per year.

For millions of payments, the total archival rent costs only fractions of a cent per transaction.

## Archival and Restoration

When the TTL of a `Persistent` entry or the `Instance` storage expires, it falls into an **Archived** state.
- **Archival**: The data is removed from the active ledger, halting contract operations that rely on it. For `RefundVault`, an archived `RefundRecord` prevents verifying double-spends natively, which is why the Soroban environment fails the transaction rather than returning "not found".
- **Restoration**: An archived entry can be restored by submitting a `RestoreFootprint` operation. Any user or agent can pay the rent to restore an archived `BatchRecord` to execute `verify_receipt` or a `RefundRecord` to interact with the vault again.

## Conclusion

The initial one-line conclusion holds true, but is now substantiated: **No state can be moved to `Temporary`**. 
- Instance state is globally required for the contracts to function.
- Persistent state (`BatchRecord` and `RefundRecord`) serve as critical audit trails and double-spend preventions that must survive indefinitely, whether active or archived.
