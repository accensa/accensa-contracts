# Accensa Contracts Event Reference

This document describes the events emitted by the `accensa-contracts`. These events form a **public interface** for indexers.

## Stability Policy
As documented in [`CONTRIBUTING.md`](../CONTRIBUTING.md), event topics and field names are guaranteed to be stable. Any modification to an event's shape or topic tuple is considered a breaking change.

When consuming these events, indexers should:
- Filter by the exact topic tuple documented below.
- Tolerate any unknown additional fields that may be appended in future non-breaking updates.

---

## `ReceiptAnchor` Events

### 1. `AnchorEvent`
Emitted when a new batch of receipts is anchored by the merchant.

- **Topics**: `("anchor_event", batch_id: u64)`
- **Data Map**:
  - `root` (`BytesN<32>`): The Merkle root of the batch.
  - `count` (`u32`): Number of receipts in the batch.
  - `period_start` (`u64`): Start time of the batch period.
  - `period_end` (`u64`): End time of the batch period.
  - `anchored_ledger` (`u32`): The ledger sequence when the batch was anchored.

*Note: The data map is structurally identical to the `BatchRecord` returned by `get_batch`.*

### 2. `PruneEvent`
Emitted when old batches are pruned to reclaim rent.

- **Topics**: `("prune_event", start_batch_id: u64)`
- **Data Map**:
  - `end_batch_id` (`u64`): The upper bound (inclusive) of the pruned range.

---

## `RefundVault` Events

### 3. `DepositEvent`
Emitted when the merchant tops up the vault's float.

- **Topics**: `("deposit_event", from: Address)`
- **Data Map**:
  - `amount` (`i128`): The amount deposited (in the token's smallest unit).

### 4. `RefundEvent`
Emitted when a payment is refunded to an agent.

- **Topics**: `("refund_event", payment_ref: BytesN<32>)`
- **Data Map**:
  - `amount` (`i128`): The amount refunded (in the token's smallest unit).
  - `recipient` (`Address`): The address that received the refund.
  - `ledger` (`u32`): The ledger sequence when the original payment occurred.

*Note: The data map is structurally identical to the `RefundRecord` returned by `get_refund`.*

### 5. `BatchRefundEvent`
Emitted once per `process_batch` call instead of one `RefundEvent` per item.
Keeping the batch to a single compact event is what lets 50+ refunds fit inside
a transaction's 16 KiB contract-event budget (a per-refund event would cap
batches at ~30).

- **Topics**: `("batch_refund_event",)`
- **Data Map**:
  - `payment_refs` (`Vec<BytesN<32>>`): The payment refs, in submission order.
  - `results` (`Vec<bool>`): Per-item outcome, aligned 1:1 with `payment_refs`
    (`true` = refund executed; `false` = item failed validation and was skipped).

*Per-item outcomes are not persisted in the event; call `get_refund(payment_ref)`
to inspect a refund record.*

### 6. `WithdrawEvent`
Emitted when the merchant withdraws funds from the float.

- **Topics**: `("withdraw_event", to: Address)`
- **Data Map**:
  - `amount` (`i128`): The amount withdrawn (in the token's smallest unit).

### 7. `PauseEvent`
Emitted when the merchant pauses the vault, halting deposits, refunds and withdrawals.

- **Topics**: `("pause_event", ledger: u32)`
- **Data Map**: *(empty)*

### 8. `UnpauseEvent`
Emitted when the merchant unpauses the vault.

- **Topics**: `("unpause_event", ledger: u32)`
- **Data Map**: *(empty)*

The `ledger` topic lets an indexer reconstruct pause windows from the event log alone: a vault is paused between a `pause_event` and the next `unpause_event`.

### 9. `RefundWindowUpdatedEvent`
Emitted when the merchant changes the refund window.

- **Topics**: `("refund_window_updated_event", previous_window: u32, new_window: u32)`
- **Data Map**: *(empty)*

Both values are carried so a reader can tell whether a refund rejected at a given ledger was rejected under the old rule or the new one.
