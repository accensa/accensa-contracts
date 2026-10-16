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

- [x] Document the 5 public fields of `AnchorEventPayload` (#772)

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

