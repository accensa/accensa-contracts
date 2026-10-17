# Roadmap

## Issue #443: stACC liquid staking derivative minting

- [x] Implement stACC liquid staking derivative minting (#443)
  - New `liquid_staking` module in `contracts/governance/src/liquid_staking.rs`
  - 1:1 mint/burn of stACC against locked underlying tokens
  - Exchange rate progression and lock-epoch-based redemption
  - CHANGELOG.md entry added

## Issue #670: iterate members in the governance constructor

- [x] `Governance::__constructor` iterates `members`/`deposits` instead of `get(i).unwrap()` (#670)

## Issue #749: rustdoc for receipt-shard's 4 exported names

- [x] Document `DataKey`, `ReceiptShard`, `get_batch` and `extend_batch_ttl` (#749)
  - `contracts/receipt-shard/src/lib.rs` — doc comments only, no signature, body or test changes

## Issue #790: rustdoc for `BuybackConfiguredEvent`'s 5 public fields

- [x] Document the struct's 5 public fields (#790)
  - `contracts/treasury/src/buyback.rs` — doc comments only, field/type/order/derives unchanged
## Issue #663: Reconcile immutability claims with refund-vault's upgrade_wasm

- [x] Reconcile immutability claims with refund-vault's upgrade_wasm (#663)
  - Updated `docs/ADR-003-upgradeability.md`, `docs/SECURITY_MODEL.md`, `docs/AUDIT.md`, and `docs/MAINNET_DEPLOYMENT.md`
  - Documented RefundVault's admin-gated two-phase upgrade flow and ReceiptAnchor's immutability

## Issue #664: Remove the fictional refund-window enforcement from SECURITY_MODEL

- [x] Remove the fictional refund-window enforcement from SECURITY_MODEL (#664)
  - Reconciled zero-window semantics in `docs/SECURITY_MODEL.md` to reflect unbounded window and TTL handling
  - Aligned `docs/storage-audit.md` and removed references to non-existent `set_refund_window`, `InvalidWindow`, and `MIN_REFUND_WINDOW`

## Issue #665: Fix CONFORMANCE.md build-vectors paths to the receipt-anchor crate

- [x] Fix CONFORMANCE.md build-vectors paths to the receipt-anchor crate (#665)
  - Corrected generator and generated file paths in `docs/CONFORMANCE.md` to `contracts/receipt-anchor/`
  - Verified `node contracts/receipt-anchor/scripts/build-vectors.mjs --check` matches CI

## Issue #758: rustdoc for `common/events`' 8 undocumented action symbols

- [x] Document `deposit`, `withdraw`, `refund`, `transfer`, `open`, `close`, `dispute` and `claim` (#758)
  - `contracts/common/src/events.rs` — doc comments only, no signature, body or ordering change

## Issue #759: rustdoc for refund-vault/lib's 8 exported names

- [x] Document `get_pending_policy`, `get_policy_timelock`, `get_refund`, `set_settlement_contract`, `get_claim_cooldown`, `set_fee_recipient`, `extend_refund_ttl` and `transfer_admin` (#759)
  - `contracts/refund-vault/src/lib.rs` — doc comments only; the 4 fallible setters state the pre-condition and the `Error` arm their body returns, the 4 getters are marked infallible

## Issue #760: rustdoc for the remaining 5 action symbols in `common/events`

- [x] Document `settle`, `anchor`, `prune`, `pause` and `unpause` (#760)
  - `contracts/common/src/events.rs` — doc comments only, inside the lines #760 names and nowhere else in the file

## Issue #769: rustdoc for `FallbackDisputeRequestedEvent`'s 6 public fields

- [x] Document `dispute_id`, `oracle`, `payment_ref`, `merchant`, `recipient` and `amount` (#769)
  - `contracts/refund-policy-vdf/src/fallback.rs` — doc comments only; field, type, order and derives unchanged, and no unit invented for `amount`

## Issue #678: error arm for absent state in receipt-shard::has_live_records

- [x] `has_live_records` returns `Result<bool, Error>` with `Error::NotInitialized` on missing state (#678)
  - `contracts/receipt-shard/src/consolidation.rs` — returns typed error instead of unwrap on missing `StartBatchId` or `EndBatchId`

## Issue #753: rustdoc for upto-authorization/lib's 4 exported names

- [x] Document `Error`, `DataKey`, `UptoAuthorization` and `initialize` (#753)
  - `contracts/upto-authorization/src/lib.rs` — doc comments only, no signature, body or test changes


