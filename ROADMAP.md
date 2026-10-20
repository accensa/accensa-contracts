# Roadmap

## Issue #708: refund-vault coupon and liquidity test coverage

- [x] Add refund-vault coupon and liquidity test coverage (#708)

## Issue #709: refund-vault integration test coverage

- [x] Cover NFT transfers, emergency yield exit, and yield distribution (#709)

## Issue #443: stACC liquid staking derivative minting

- [x] Implement stACC liquid staking derivative minting (#443)
  - New `liquid_staking` module in `contracts/governance/src/liquid_staking.rs`
  - 1:1 mint/burn of stACC against locked underlying tokens
  - Exchange rate progression and lock-epoch-based redemption
  - CHANGELOG.md entry added

## Documentation: rustdoc field docs for contracttype/event payloads

- [x] Document the publicly exported fields of `RefundParam` in refund-vault (#784)
- [x] Document the publicly exported fields of `YieldInfo` in refund-vault (#785)
- [x] Document the publicly exported fields of `CouponAppliedEvent` in refund-vault (#783)
- [x] Document the publicly exported fields of `HtlcAddedEvent` in state-channel (#786)
- [x] Document the publicly exported fields of `OptimisticQueuedEvent` in governance (#775)
## Issue #660: PR template verification step runs the full workspace suite

- [x] Point the PR template's Verification checkbox at `cargo test --workspace` (#660)

## Issue #658: CHANGELOG's pre-1.0 versioning policy matches RELEASING.md

- [x] State the pre-1.0 minor-for-breaking rule once in `docs/RELEASING.md` and cite it from `CHANGELOG.md` (#658)

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

## Issue #650: Cover the reputation badge authority guard's two failure arms

- [x] Pin the not-initialised, wrong-caller and admitted arms of the reputation authority guard (#650)
  - `contracts/reputation/src/badges_test.rs` — test-only additions; asserts `NotInitialized`, the host `Abort` for a wrong caller, and the admitted admin's persisted tier

## Issue #683: error arm for absent state in refund-vault::get_domain_separator

- [x] `get_domain_separator` returns `Result<BytesN<32>, Error>` with `Error::NotInitialized` on missing state (#683)
  - `contracts/refund-vault/src/lib.rs` — returns typed error instead of unwrap on missing `DomainSeparator`

## Issue #649: Return a typed error when the yield float cannot cover a claim

- [x] Return `Error::InsufficientBalance` when yield float is insufficient (#649)
  - `contracts/treasury/src/distribution.rs` — verified contract holding balance of yield token before mutating user `pending`/`claimed`
  - Added unit test in `contracts/treasury/src/distribution.rs` asserting typed error and unchanged pending yield

## Issue #722: guard `preview_settlement`'s cumulative sum against overflow

- [x] Route the `previous_refunded + amount` sum in `preview_settlement` through `add_amounts` (#722)
  - `contracts/refund-vault/src/settlement.rs` — typed `Error::MathOverflow` instead of a host abort
  - `contracts/refund-vault/src/preview_settlement_overflow_tests.rs` — boundary test pins both operands at `i128::MAX` and asserts the returned error variant

## Issue #720: check `harvest_yield`'s running total against overflow

- [x] Route the `harvested + yield_amount` sum in `harvest_yield` through `add_amounts` (#720)
  - `contracts/refund-vault/src/lib.rs` — typed `Error::MathOverflow` instead of a host abort
  - `contracts/refund-vault/src/harvest_yield_overflow_tests.rs` — boundary test pins the stored total at `i128::MAX` and asserts the returned error variant


## Issue #685: report a missing payment token from `deploy_to_yield`

- [x] Return `Error::NotInitialized` when `DataKey::Token` is unset in `deploy_to_yield` (#685)
  - `contracts/refund-vault/src/lib.rs` — typed error instead of a host abort
  - `contracts/refund-vault/src/deploy_to_yield_token_tests.rs` — test removes the stored token and asserts the returned error variant

## Issue #686: report a missing refund window from `extend_refund_ttl`

- [x] Return `Error::NotInitialized` when `DataKey::RefundWindow` is unset in `extend_refund_ttl` (#686)
  - `contracts/refund-vault/src/lib.rs` — typed error instead of a host abort
  - `contracts/refund-vault/src/extend_refund_ttl_window_tests.rs` — test stores a refund record, removes the window and asserts the returned error variant

## Issue #687: walk the fee ladder in `tier_for_volume` without indexing

- [x] Iterate the ladder in `tier_for_volume` instead of `ladder.get(i).unwrap()` (#687)
  - `contracts/refund-vault/src/tiers.rs` — enumerated iteration, no trap path, unchanged behaviour

## Issue #688: resolve `next_threshold` from the bound `Option`

- [x] Match on `ladder.get(next)` in `next_threshold` instead of unwrapping (#688)
  - `contracts/refund-vault/src/tiers.rs` — the `None` arm is the documented top-rung `i128::MAX`, unchanged behaviour


## Issue #655: Assert the treasury vesting schedule and claim event shapes

- [x] Pin `ScheduleCreatedEvent` and `ClaimedEvent` topics and payloads, and the silent zero-payout claim (#655)
  - `contracts/treasury/tests/vesting_events.rs` — integration test file, no contract logic changed

## Issue #657: Cover reputation SBT authority rejection for issue, revoke and slash

- [x] Assert unauthorized `issue`, `revoke` and `slash` calls are rejected and leave state untouched (#657)
  - `contracts/reputation/src/sbt_test.rs` — negative-path cases, no contract logic changed

## Issue #659: Fix CONTRIBUTING build/test commands to cover non-default crates

- [x] Use `--workspace` for the local build and test examples so they match CI (#659)
  - `CONTRIBUTING.md` — build/test commands plus the note on the narrower `default-members`

## Issue #661: Update stale 0.2.0 version statements to the released 0.3.0

- [x] Point the supported-versions table at the 0.3.x release line (#661)
  - `SECURITY.md` — supported and unsupported version rows; the 0.1.0 testnet note is unchanged

## Issue #787: rustdoc for `ChannelOpenedEvent`'s 5 public fields

- [x] Document the struct's 5 public fields (#787)
  - `contracts/state-channel/src/lib.rs` — doc comments only, no signature, body, order or derive changes

## Issue #788: rustdoc for `MultiAssetChannel`'s 5 public fields

- [x] Document `sender`, `receiver`, `phase`, `opened_at` and `challenge_period` (#788)
  - `contracts/state-channel/src/multi_asset.rs` — doc comments only, field/type/order/derives unchanged

## Issue #792: rustdoc for `AuthorizeEvent`'s 5 public fields

- [x] Document `payment_id`, `from`, `to`, `cap` and `expiry` (#792)
  - `contracts/upto-authorization/src/lib.rs` — doc comments only, field/type/order/derives unchanged

## Issue #750: Document 4 public items in nft_escrow

- [x] Add `///` rustdoc to `deposit`, `claim`, `refund` and `get` (#750)
  - `contracts/refund-vault/src/nft_escrow.rs` — doc comments only; role, pre-conditions and each `Error` arm documented, signatures and bodies unchanged

## Issue #729: package description for accensa-common

- [x] Add `description` to the `[package]` table of `contracts/common/Cargo.toml` (#729)

## Issue #730: package description for multisig-account

- [x] Add `description` to the `[package]` table of `contracts/multisig-account/Cargo.toml` (#730)

## Issue #731: package description for receipt-shard

- [x] Add `description` to the `[package]` table of `contracts/receipt-shard/Cargo.toml` (#731)

## Issue #732: package description for testutils

- [x] Add `description` to the `[package]` table of `contracts/testutils/Cargo.toml` (#732)

## Issue #751: Document 4 public items in multi_asset

- [x] Add `///` rustdoc to `MultiAssetChannelOpenedEvent`, `MultiAssetSettledEvent`, `open` and `get` (#751)
  - `contracts/state-channel/src/multi_asset.rs` — doc comments only; role, pre-conditions and each `Error` arm documented, signatures and bodies unchanged
