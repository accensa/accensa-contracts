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

### 5. `WithdrawEvent`
Emitted when the merchant withdraws funds from the float.

- **Topics**: `("withdraw_event", to: Address)`
- **Data Map**:
  - `amount` (`i128`): The amount withdrawn (in the token's smallest unit).

### 6. `PauseEvent`
Emitted when the merchant pauses the vault, halting deposits, refunds and withdrawals.

- **Topics**: `("pause_event", ledger: u32)`
- **Data Map**: *(empty)*

### 7. `UnpauseEvent`
Emitted when the merchant unpauses the vault.

- **Topics**: `("unpause_event", ledger: u32)`
- **Data Map**: *(empty)*

The `ledger` topic lets an indexer reconstruct pause windows from the event log alone: a vault is paused between a `pause_event` and the next `unpause_event`.

### 8. `RefundWindowUpdatedEvent`
Emitted when the merchant changes the refund window.

- **Topics**: `("refund_window_updated_event", previous_window: u32, new_window: u32)`
- **Data Map**: *(empty)*

Both values are carried so a reader can tell whether a refund rejected at a given ledger was rejected under the old rule or the new one.

### 9. `OraclePolicySetEvent`
Emitted when the merchant installs (or replaces) the dynamic oracle policy
that gates refunds.

- **Topics**: `("oracle_policy_set_event", feed_id: BytesN<32>)`
- **Data Map**:
  - `threshold` (`i128`): The median value (in the feed's scale) at which the condition flips.
  - `refund_when_below` (`bool`): `true` = refunds allowed while the median is strictly below the threshold; `false` = allowed while strictly above.
  - `max_staleness_ledgers` (`u32`): Maximum allowed age of a feed value; `0` = never stale.

The data map carries the full condition, so an indexer can reconstruct the
policy in force from the event log alone.

### 10. `OraclePolicyClearedEvent`
Emitted when the merchant removes the dynamic oracle policy, restoring purely
time-window-based refunds.

- **Topics**: `("oracle_policy_cleared_event", feed_id: BytesN<32>)`
- **Data Map**: *(empty)*

The `feed_id` is the feed of the policy that was in force, captured before it
was removed, so a reader can correlate the clear with the preceding set event.
