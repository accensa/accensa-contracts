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

## Issue #644: Fail closed when DataKey::Threshold is missing from instance storage

- [x] Read `DataKey::Threshold` with `ok_or(...)` in `__check_auth` instead of `unwrap_or(1)` so a lapsed entry rejects authorization rather than lowering the threshold to 1 (#644)

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

## Issue #683: error arm for absent state in refund-vault::get_domain_separator

- [x] `get_domain_separator` returns `Result<BytesN<32>, Error>` with `Error::NotInitialized` on missing state (#683)
  - `contracts/refund-vault/src/lib.rs` — returns typed error instead of unwrap on missing `DomainSeparator`

## Issue #692: report a missing token with a typed error

- [x] Report a missing token address with `Error::NotInitialized` (#692)
  - `contracts/stream-vault/src/lib.rs` — `token_client` returns `Error::NotInitialized` instead of unwinding when the `Token` key is absent

## Issue #662: Correct the soroban-sdk version quoted in SUPPORT.md

- [x] Correct the soroban-sdk version quoted in SUPPORT.md (#662)
  - Updated `SUPPORT.md` to reference `soroban-sdk` 27.0.6 matching the resolved workspace pin
  - Cited `Cargo.lock` as the single source of truth for the SDK version

## Issue #690: read the selected tier without unwrapping

- [x] `set_tier_ladder` uses a checked lookup for the selected tier's fee (#690)
  - `contracts/refund-vault/src/tiers.rs` — `set_tier_ladder` performs a checked `tiers.get(current_tier)` lookup with `Error::InvalidTierLadder` as a defensive fallback when the preserved volume points past the last rung; validated ladders retain their existing behaviour.

## Issue #679: receipt-shard::prune_batches traps on an uninitialised Router

- [x] `prune_batches` answers `Error::NotInitialized` instead of aborting the host call when `Router`, `EndBatchId` or `PrunedUpTo` is absent (before construction or after the instance entry ages out) (#679)

## Issue #745: rustdoc for reentrancy's 4 exported names

- [x] Document `ReentrancyDataKey`, `ReentrancyGuard`, `acquire` and `release` (#745)
  - `contracts/common/src/reentrancy.rs` — doc comments only, no signature, body or test changes

## Issue #744: rustdoc for receipt-anchor/lib's 6 exported names

- [x] Document `DataKey`, `ReceiptAnchor`, `initialize`, `get_batch`, `verify_receipt` and `get_shard_capacity` (#744)
  - `contracts/receipt-anchor/src/lib.rs` — doc comments only; `initialize` states its no-auth/once pre-condition and `Error::AlreadyInitialized` arm, `get_batch` and `verify_receipt` state the `BatchNotFound`/`ProofTooLong`/`ShardCallFailed` arms, and the getter is marked infallible

## Issue #728: guard state-channel::add's reservation sum against overflow

- [x] Compute the new HTLC reservation with `add_amounts` so an out-of-range sum returns `Error::MathOverflow` instead of trapping (#728)
  - `contracts/state-channel/src/htlc.rs` — the reservation update uses the shared checked helper; the escrow ceiling above it remains the precondition that keeps every reachable sum in range

## Issue #727: guard state-channel::hashlock::add's reservation sum against overflow

- [x] Compute the new hashlock reservation with `add_amounts` so an out-of-range sum returns `Error::MathOverflow` instead of trapping (#727)
  - `contracts/state-channel/src/hashlock.rs` — the reservation update uses the shared checked helper; the escrow ceiling above it remains the precondition that keeps every reachable sum in range

## Issue #724: guard `apply_decay`'s final addition against overflow

- [x] `apply_decay` guards `SCORE_FLOOR + decayed` with `checked_add` and states the precondition that keeps the sum in range (#724)

## Issue #723: guard `recall_principal`'s sums against overflow

- [x] Route both `recall_principal` sums through `add_amounts` (#723)
  - `contracts/refund-vault/src/strategy.rs` — typed `Error::MathOverflow` instead of a host abort
  - `contracts/refund-vault/src/recall_principal_overflow_tests.rs` — boundary tests drive both sums to `i128::MAX`

## Issue #722: guard `preview_settlement`'s cumulative sum against overflow

- [x] Route the `previous_refunded + amount` sum in `preview_settlement` through `add_amounts` (#722)
  - `contracts/refund-vault/src/settlement.rs` — typed `Error::MathOverflow` instead of a host abort
  - `contracts/refund-vault/src/preview_settlement_overflow_tests.rs` — boundary test pins both operands at `i128::MAX` and asserts the returned error variant
