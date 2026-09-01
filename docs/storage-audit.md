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
| `RateLimitConfig` | Instance | `RateLimitConfig` (`burst_capacity`, `refill_interval_secs`) | 8 bytes | Admin-configured token-bucket rate limit for `anchor_batch`. `{0, 0}` (the default) disables rate limiting. |
| `RateLimitBucket(Address)` | Persistent | `BucketState` (`tokens`, `last_refill`) | ~12 bytes | Per-identity token-bucket state, keyed by the anchoring identity (the merchant). Written only while a rate limit is enabled; the sole tracking storage the limiter needs. TTL is extended on every successful anchor so an active identity's bucket never archives mid-burst. If archived, the next anchor treats the bucket as fresh and full, which is a conservative (rate-limiting) outcome. |
| `Batch(u64)` | Persistent | `BatchRecord` | ~100 bytes | Holds the Merkle root, count, period, and ledger. Required for on-chain `verify_receipt` execution. While `AnchorEvent` emits this data, on-chain functions cannot read events. Must be persistent to prevent arbitrary deletion; if archived, it can be restored to prove old receipts. |

### `RefundVault`

| DataKey | Class | Contents | Size | Justification |
|---|---|---|---|---|
| `Admin` | Instance | `Address` (Merchant) | Small | Required for authentication of merchant operations (`deposit`, `refund`, `withdraw`, `pause`). |
| `Token` | Instance | `Address` (SEP-41 token contract; the USDC SAC by default) | Small | The underlying asset contract address. The vault is token-agnostic — any SEP-41 token is accepted — but each vault instance is bound to exactly one token. Crucial for token transfers. |
| `RefundWindow` | Instance | `u32` (Ledgers) | Small | Global policy parameter determining refund eligibility. |
| `RefundDeadline` | Instance | `u64` (Unix seconds) | Small | Wall-clock deadline after which refund claims are rejected; `0` = no deadline. Set together with the window by `propose_policy`/`execute_policy` and consulted at claim time in `refund`. |
| `FeeBps` | Instance | `u32` | Small | Refund fee rate in basis points (`0` = no fee, max `10_000`); consulted at claim time by `refund` and updated by `set_fee_bps`. |
| `FeeRecipient` | Instance | `Address` (Optional) | Small | Explicit fee collector; when unset, `refund` pays the fee to the merchant (admin). Set by `set_fee_recipient`. |
| `IsPaused` | Instance | `bool` | Small | Emergency halt flag. Must be immediately available at all times. |
| `Metadata` | Instance | Reserved | Variable | Reserved for future contract configuration or metadata. |
| `RefundMax` | Instance | `i128` | Small | Reserved configuration for maximum allowed refund limits. |
| `Admins` | Instance | Reserved | Variable | Reserved for potential multi-admin expansion. |
| `Threshold` | Instance | Reserved | Small | Reserved for potential multi-sig or quorum thresholds. |
| `Refund(BytesN<32>)`| Persistent | `RefundRecord` | ~100 bytes | Legacy (0.1.0) single-refund record, retained read-only for migration detection. |
| `RefundV2(BytesN<32>)`| Persistent | `RefundRecord` | ~100 bytes | Tracks cumulative refunds per payment (amount, recipient, ledger). Critical to prevent replay attacks (double-refunding the same payment). If this were Temporary, it could expire and allow a second refund. If archived, it remains a tombstone that prevents re-creation until restored — see "TTL Strategy" below for why its TTL extension is sized to the configured refund window rather than a flat interval, and why the threshold passed to `extend_ttl` matters as much as the extension amount. |
| `YieldStrategy` | Persistent | `Address` | Small | Address of the external yield strategy contract. Only loaded by yield-related calls (`deploy_to_yield`, `withdraw_from_yield`, `harvest_yield`, `get_yield_info`). Kept in Persistent (not Instance) storage so non-yield calls (`deposit`, `refund`, `withdraw`, `pause`) never pay the read/write byte cost of loading it (issue #131). Extended with `TTL_EXTEND` on every write. |
| `DeployedPrincipal` | Persistent | `i128` | Small | Cumulative principal deployed to the yield strategy. Only loaded by yield calls. Kept in Persistent storage to avoid loading cost on non-yield calls (issue #131). Extended with `TTL_EXTEND` on every write. |
| `HarvestedYield` | Persistent | `i128` | Small | Cumulative yield harvested from the strategy, tracked for operator withdrawal. Only loaded by yield calls. Kept in Persistent storage to avoid loading cost on non-yield calls (issue #131). Extended with `TTL_EXTEND` on every write. |
| `ReserveRatio` | Persistent | `u32` (basis points) | Small | Minimum reserve ratio in basis points (e.g. 2000 = 20%). Determines how much liquid balance must remain after yield deployment. Only loaded by yield calls. Kept in Persistent storage to avoid loading cost on non-yield calls (issue #131). Extended with `TTL_EXTEND` on every write. |
| `MaxDeployRatio` | Persistent | `u32` (basis points) | Small | Maximum deployment ratio in basis points (e.g. 8000 = 80%). Caps the total deployed principal relative to total vault value. Only loaded by yield calls. Kept in Persistent storage to avoid loading cost on non-yield calls (issue #131). Extended with `TTL_EXTEND` on every write. |
| `PendingPolicy` | Instance | `PolicyProposal` | Small | A pending refund-window policy change waiting for its timelock to expire. |
| `ReentrancyLock` | Instance | `bool` | Small | Transient guard flag set during external calls (token transfers, strategy invocations) to reject reentrant calls. |

*Note: The `Metadata`, `RefundMax`, `Admins`, and `Threshold` keys are defined in the `DataKey` enum for future compatibility and expansion, though some may currently be inactive in the logic.*

### Token Generality

`RefundVault` is deliberately token-agnostic. `initialize` binds one instance to one token contract, and the vault never assumes anything about that token beyond SEP-41. In particular it does **not** assume seven decimals: all amounts (`deposit`, `refund`, `withdraw`) are raw integer units in the token's smallest unit, and the float-bound check compares those units directly against the vault's token balance. A 0- or 2-decimal SEP-41 token therefore behaves identically to a 7-decimal Stellar Asset Contract — the vault performs no decimal arithmetic of its own. Converting human-readable amounts into the token's smallest unit is the responsibility of the merchant and the facilitator, not the contract.

This matches the conclusion in `accensa-app` (the facilitator): one vault is bound to one token, so a merchant settling in multiple assets deploys one vault per asset. The full lifecycle (deposit → refund → withdraw) and the float-bound check are exercised against a non-7-decimal token in `token_agnostic_tests.rs`, along with the smallest unit, `i128` extremes, and a refund exactly equal to the float.

## TTL Strategy

Stellar uses a Time-To-Live (TTL) mechanism to manage state bloat.

- **`TTL_EXTEND`**: `518,400` ledgers (approximately 30 days, assuming ~5 seconds per ledger).
- **`TTL_THRESHOLD`**: `100` ledgers.

**Rationale**:
A 30-day `TTL_EXTEND` ensures that actively used batches and recent refund records remain in the live state without requiring manual restoration by downstream clients. The `TTL_THRESHOLD` of 100 ledgers acts as a buffer to prevent rent-bumping transactions from spamming the network on every single contract call—only extending the TTL if it drops below this threshold.

Both `Instance` storage (which covers `Admin`, `IsPaused`, etc.) and the actively modified `Persistent` entries (`RefundRecord`, `BatchRecord`, yield-related keys) receive TTL extensions during mutations to keep the active working set alive.

**`RefundVault`'s `RefundV2` guard is a deliberate exception to the flat `TTL_THRESHOLD`/`TTL_EXTEND` pattern above**, for two reasons discovered while verifying the double-refund guard against real archival behaviour:

1. *A flat 30-day extension doesn't track the configured refund window.* `refund` only re-checks `has()`/`get()` on this key when it is called; nothing re-extends its TTL between calls. A merchant with `refund_window_ledgers` longer than 518,400 (or `0`, meaning "no time bound" — `set_refund_window`/`initialize` deliberately allow this), who issues one partial refund near the start of that window and nothing else, would have had a guard entry whose TTL could lapse well before the window itself closes — even though further `refund` calls against that `payment_ref` are still policy-valid. `refund_record_ttl_extend_to` in `contracts/refund-vault/src/lib.rs` now sizes the extension to `paid_at_ledger + window` (or the network's `max_ttl()` when `window == 0`), so the guard cannot outlive its own policy window and cannot age out while it does.
2. *`TTL_THRESHOLD` (100 ledgers, ~8 minutes) is below any realistic `min_persistent_entry_ttl` floor*, including the SDK's own `4096`-ledger test default. Since `extend_ttl(threshold, extend_to)` only bumps the TTL when the entry's *current* remaining TTL is below `threshold`, and a freshly-written persistent entry already carries the network's floor TTL (which exceeds 100 on any real network), the `extend_ttl(TTL_THRESHOLD, TTL_EXTEND)` call used elsewhere in this contract is a no-op immediately after `set` — the entry is left at the network floor, not at `TTL_EXTEND`. For the `RefundV2` key (and its manual top-up, `extend_refund_ttl`), the threshold passed is the *computed `extend_to` value itself* (`extend_ttl(extend_to, extend_to)`), so the extension actually fires whenever the entry's TTL is below what the policy requires. This is proven in `contracts/refund-vault/src/test.rs` (`test_long_window_extends_guard_past_flat_ttl`, `test_zero_window_extends_guard_to_max_ttl`), which fail against the old flat-threshold code and pass against the fix.

**Yield-related persistent keys** (`YieldStrategy`, `DeployedPrincipal`, `HarvestedYield`, `ReserveRatio`, `MaxDeployRatio`) follow the standard flat `TTL_EXTEND`/`TTL_THRESHOLD` pattern via the `persist_yield_ttl` helper. These keys are only written by yield-related entry points (`set_yield_strategy`, `set_reserve_ratio`, `set_max_deploy_ratio`, `deploy_to_yield`, `withdraw_from_yield`, `harvest_yield`), and non-yield calls (`deposit`, `refund`, `withdraw`, `pause`, admin transfer) never touch them — which is the entire point of moving them out of Instance storage (issue #131). The flat extension is sufficient here because the yield configuration is purely admin-set and does not need to track a time-bound policy window the way `RefundV2` does.

This does not fully resolve the open question of whether an archived (not just aged) persistent entry fails safe (host traps on access) or fails open on the live network — `docs/SECURITY_MODEL.md` still flags that as verified only against this SDK's test host, which auto-heals expired entries rather than modeling a hard archival trap. What this fix removes is the case where the guard's TTL falls short of the policy window on its own, regardless of how archival itself behaves.

## Rent Cost Implications

Persistent storage incurs rent to stay active on the Stellar network, priced at roughly **0.5 XLM per KB per year**.

- **BatchRecord**: A single record is roughly 100 bytes (root: 32b, count: 4b, periods: 16b, overhead: ~50b).
- **RefundRecord**: A single record is roughly 100 bytes (address: 32b, amount: 16b, ledger: 4b, overhead: ~50b).
- **Yield config keys** (`YieldStrategy`, `DeployedPrincipal`, `HarvestedYield`, `ReserveRatio`, `MaxDeployRatio`): Fixed set of 5 entries, ~50 bytes total. Only written by yield operations; no per-payment scaling.

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
