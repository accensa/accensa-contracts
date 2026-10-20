# Roadmap

## Issue #743: rustdoc for liquid_staking's 6 public items

- [x] Document `LiquidStakingError`, `Mint`, `Burn`, `ExchangeRate`, `UserData` and `LiquidStaking` (#743)
  - `contracts/governance/src/liquid_staking.rs` — doc comments only; each item states its role, no signature, body or reorder change, and `missing_docs` is deliberately not enabled
## Issue #780: rustdoc for ShardStats's 5 exported fields

- [x] Document the struct's 5 public fields (#780)
  - `contracts/receipt-shard/src/diagnostics.rs` — doc comments only, field/type/order/derives unchanged

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
- [x] Document the publicly exported fields of `ProposalCreated` in governance (#774)
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

## Issue #725: guard `record_completion`'s growth product and sum against overflow

- [x] `record_completion` computes its growth with `checked_mul` and lands the score with `checked_add`; `headroom` is capped by `MAX_SCORE` and `growth <= headroom`, so the failure arms state those preconditions and no error variant is added (#725)

## Issue #722: guard `preview_settlement`'s cumulative sum against overflow

- [x] Route the `previous_refunded + amount` sum in `preview_settlement` through `add_amounts` (#722)
  - `contracts/refund-vault/src/settlement.rs` — typed `Error::MathOverflow` instead of a host abort
  - `contracts/refund-vault/src/preview_settlement_overflow_tests.rs` — boundary test pins both operands at `i128::MAX` and asserts the returned error variant

## Issue #696: cover `governance::burn` and the ragequit path

- [x] Cover `LiquidStaking::burn`, `ragequit::{is_approved, process}` and the `ArityMismatch`/`InvalidMembers` constructor variants (#696)
  - `contracts/governance/tests/receipt_anchor_admin.rs` — integration tests only, no `src/` changes

## Issue #697: cover `receipt-anchor::{topic, publish, canonical_message}`

- [x] Cover `ReceiptAction::topic`, `events::publish` and `signatures::canonical_message` (#697)
  - `contracts/receipt-anchor/src/budget_test.rs` — tests only, no contract source changes

## Issue #698: cover `receipt-anchor::{verify_batch_zk_proof, fold_proof, get_min_anchor_interval}`

- [x] Cover `zk_verifier::verify_batch_zk_proof`, `merkle::fold_proof` and `ReceiptAnchor::get_min_anchor_interval` (#698)
  - `contracts/receipt-anchor/src/fuzz_test.rs` — tests only, no contract source changes

## Issue #695: cover `cross-chain::{execute_withdrawal_to_evm, get_destination_chain_id, is_trusted_peer}`

- [x] Cover `outbound::execute_withdrawal_to_evm`, `get_destination_chain_id` and `is_trusted_peer` (#695)
  - `contracts/cross-chain/src/wormhole_test.rs` — tests only, no contract source changes
## Issue #652: test: Pin the oracle admin getter and the round staleness bound

- [x] Pin `get_admin` unset/bound states, the typed `AlreadyInitialized` double-initialize guard, and the inclusive staleness boundary in `contracts/oracle/src/staleness_test.rs` (#652)

## Issue #653: test: Cover the cross-chain config getters' uninitialized arms

- [x] Cover all four config getters unset/set/rebound and both `require_admin` verdicts without mocked auth in `contracts/cross-chain/src/config_readers_test.rs` (#653)

## Issue #654: test: Cover the factory's already-tracked address branch on deploy

- [x] Drive the already-tracked address collision branch and pin registry/salt invariants in `contracts/refund-vault-factory/tests/tracked_address.rs` (#654)

## Issue #656: test: Assert treasury schedule entries keep their TTL across claims

- [x] Assert schedule TTL at registration, re-bump after decay on a successful claim, and no shortening on a rejected claim in `contracts/treasury/tests/schedule_ttl.rs` (#656)
## Issue #741: rustdoc for refund-vault-factory/lib's 7 public items

- [x] Document `Error`, `RefundVaultFactory`, `get_admin`, `get_vault_wasm`, `get_time_policy_contract`, `get_vdf_policy_contract` and `get_next_salt` (#741)
  - `contracts/refund-vault-factory/src/lib.rs` — doc comments only, no signature, body or ordering change


## Issue #740: rustdoc for `common/events`' 8 module symbols

- [x] `prune_batches` answers `Error::NotInitialized` instead of aborting the host call when `Router`, `EndBatchId` or `PrunedUpTo` is absent (before construction or after the instance entry ages out) (#679)

## Issue #746: rustdoc for governance/lib's 4 undocumented public items

- [x] Document `Error`, `DataKey`, `RingSignature` and `Governance` (#746)
  - `contracts/governance/src/lib.rs` — doc comments only, no signature, body or test changes
- [x] Document `vault`, `channel`, `receipt`, `bridge`, `gov`, `treasury`, `multisig` and `auth` (#740)
  - `contracts/common/src/events.rs` — doc comments only, no signature, body or ordering change


## Issue #720: check `harvest_yield`'s running total against overflow

- [x] Route the `harvested + yield_amount` sum in `harvest_yield` through `add_amounts` (#720)
  - `contracts/refund-vault/src/lib.rs` — typed `Error::MathOverflow` instead of a host abort
  - `contracts/refund-vault/src/harvest_yield_overflow_tests.rs` — boundary test pins the stored total at `i128::MAX` and asserts the returned error variant

- [x] Document `DataKey`, `ReceiptAnchor`, `initialize`, `get_batch`, `verify_receipt` and `get_shard_capacity` (#744)
  - `contracts/receipt-anchor/src/lib.rs` — doc comments only; `initialize` states its no-auth/once pre-condition and `Error::AlreadyInitialized` arm, `get_batch` and `verify_receipt` state the `BatchNotFound`/`ProofTooLong`/`ShardCallFailed` arms, and the getter is marked infallible

## Issue #728: guard state-channel::add's reservation sum against overflow

- [x] Compute the new HTLC reservation with `add_amounts` so an out-of-range sum returns `Error::MathOverflow` instead of trapping (#728)
  - `contracts/state-channel/src/htlc.rs` — the reservation update uses the shared checked helper; the escrow ceiling above it remains the precondition that keeps every reachable sum in range

## Issue #685: report a missing payment token from `deploy_to_yield`

- [x] Return `Error::NotInitialized` when `DataKey::Token` is unset in `deploy_to_yield` (#685)
  - `contracts/refund-vault/src/lib.rs` — typed error instead of a host abort
  - `contracts/refund-vault/src/deploy_to_yield_token_tests.rs` — test removes the stored token and asserts the returned error variant

## Issue #686: report a missing refund window from `extend_refund_ttl`

- [x] Return `Error::NotInitialized` when `DataKey::RefundWindow` is unset in `extend_refund_ttl` (#686)
  - `contracts/refund-vault/src/lib.rs` — typed error instead of a host abort
  - `contracts/refund-vault/src/extend_refund_ttl_window_tests.rs` — test stores a refund record, removes the window and asserts the returned error variant

## Issue #687: walk the fee ladder in `tier_for_volume` without indexing

- [x] Route both `recall_principal` sums through `add_amounts` (#723)
  - `contracts/refund-vault/src/strategy.rs` — typed `Error::MathOverflow` instead of a host abort
  - `contracts/refund-vault/src/recall_principal_overflow_tests.rs` — boundary tests drive both sums to `i128::MAX`

## Issue #779: rustdoc for `ShardCreatedEvent`'s 5 exported fields

- [x] Document `shard_id`, `shard_index`, `shard_address`, `start_batch_id` and `end_batch_id` (#779)
  - `contracts/receipt-anchor/src/lib.rs` — doc comments only; struct layout, field types and topic tuple unchanged
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

## Issue #716: guard `calculate_slashing_penalty`'s product before it can trap

- [x] Route `calculate_slashing_penalty`'s `amount * penalty` through the checked `accensa_common::math` helpers (#716)
  - `contracts/refund-policy-vdf/src/slashing.rs` — decomposition keeps both products in range; the `penalty <= 100` precondition is the stated failure arm, no clamp
  - `contracts/refund-policy-vdf/src/slashing_overflow_tests.rs` — boundary test drives `i128::MAX` and asserts the exact penalty

## Issue #717: check the product in `refund-vault::refund_fee` against overflow

- [x] Route `refund_fee`'s fee decomposition through the checked `accensa_common::math` helpers (#717)
  - `contracts/refund-vault/src/lib.rs` — `mul_amounts`/`add_amounts`/`div_amounts` instead of bare operators; `fee_bps <= 10_000` is the stated precondition
  - `contracts/refund-vault/src/refund_fee_overflow_tests.rs` — boundary test drives `i128::MAX` at the maximum fee rate

## Issue #718: check the sum in `refund-vault::claim_single` against overflow

- [x] Route `claim_single`'s `previous_refunded + claim.amount` through `add_amounts` (#718)
  - `contracts/refund-vault/src/lib.rs` — typed `Error::MathOverflow` instead of a host abort
  - `contracts/refund-vault/src/claim_single_overflow_tests.rs` — boundary test pins the stored total at `i128::MAX - 1` and claims two more units

## Issue #719: guard `deploy_to_yield`'s product/sum before it can trap

- [x] Route `deploy_to_yield`'s totals, ratios and post-deployment sum through the checked `accensa_common::math` helpers (#719)
  - `contracts/refund-vault/src/lib.rs` — `add_amounts`/`sub_amounts`/`mul_ratio` instead of bare operators
  - `contracts/refund-vault/src/deploy_to_yield_overflow_tests.rs` — boundary tests drive the total, reserve-ratio product and max-deploy product to the type's maximum
## Issue #777: Add /// to BatchRecord's exported fields

- [x] Add `///` doc comments to `BatchRecord`'s five exported fields (#777)
  - Documented `root`, `count`, `period_start`, `period_end`, and `anchored_ledger` in `contracts/receipt-anchor/src/lib.rs`
  - Verified `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked --workspace`, and rustdoc rendering
## Issue #787: rustdoc for `ChannelOpenedEvent`'s 5 public fields

- [x] Document the struct's 5 public fields (#787)
  - `contracts/state-channel/src/lib.rs` — doc comments only, no signature, body, order or derive changes

## Issue #788: rustdoc for `MultiAssetChannel`'s 5 public fields

- [x] Document `sender`, `receiver`, `phase`, `opened_at` and `challenge_period` (#788)
  - `contracts/state-channel/src/multi_asset.rs` — doc comments only, field/type/order/derives unchanged

## Issue #792: rustdoc for `AuthorizeEvent`'s 5 public fields

- [x] Document `payment_id`, `from`, `to`, `cap` and `expiry` (#792)
  - `contracts/upto-authorization/src/lib.rs` — doc comments only, field/type/order/derives unchanged

## Issue #666: Update ARCHITECTURE.md beyond the two-contract framing

- [x] Broaden `docs/ARCHITECTURE.md`'s scope statement and add the full workspace contract inventory (#666)
  - `docs/ARCHITECTURE.md` — new intro scope, `Contract Inventory` table derived from `Cargo.toml` members, and the `Cross-Contract Relationship` opener; walkthrough unchanged

## Issue #667: Back RELEASING's reproducible-build claim with a pinned toolchain

- [x] Scope `docs/RELEASING.md`'s reproducibility claim to source-to-commit provenance and document the toolchain (#667)
  - `docs/RELEASING.md` — step 3 now states what is not byte-reproducible without a pinned compiler and how to record `rustc`/`cargo` versions

## Issue #668: Delete the stray duplicate Unreleased section at CHANGELOG's end

- [x] Remove the duplicate trailing `## [Unreleased]` heading and stub from `CHANGELOG.md` (#668)
  - `CHANGELOG.md` — only the top `## [Unreleased]` remains; the link-reference block is the final section
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

## Issue #776: Document the public fields of `PrunePayload`

- [x] Add `///` rustdoc to `schema_version`, `timestamp`, `shard_id`, `start_batch_id` and `end_batch_id` (#776)
  - `contracts/receipt-anchor/src/events.rs` — one doc line per field stating its domain and the `prune_batches` call site that writes it; field/type/order/derives unchanged
