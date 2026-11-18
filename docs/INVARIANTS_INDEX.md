# Invariants Index

A single map from each contract's enforced invariants to the code path that
enforces them and the test that covers them. The two original contracts
(`RefundVault`, `ReceiptAnchor`) are also summarised in the
[README](../README.md#enforced-invariants--test-coverage-mapping); this index
extends that matrix to the six other production contracts.

Test references are `contracts/<crate>/src/<file>` -> test function name. When a
test is renamed, update the row in the same change.

## `RefundVault` and `ReceiptAnchor`

See the README matrix for the full list. Summary:

| Contract | Invariant | Enforcement path | Test reference |
|---|---|---|---|
| RefundVault | A `payment_ref` is refunded at most once (`AlreadyRefunded`) | `refund` | `refund-vault/src/test.rs` -> `test_process_batch_mixed_success_failure` (exercises `AlreadyRefunded`) |
| RefundVault | Refunds past `refund_window_ledgers` are rejected (`WindowExpired`) | `refund` | `refund-vault/src/test.rs` -> `test_refund_outside_window_fails` |
| RefundVault | A refund or withdrawal never exceeds the vault float (`InsufficientFloat`) | `refund`, `withdraw` | `refund-vault/src/test.rs` -> `test_refund_exceeding_float_fails` |
| RefundVault | Operations halt while paused (`Paused`) | `pause` / `unpause` | `refund-vault/src/test.rs` -> `test_paused_state_blocks_and_preserves_every_operation` |
| ReceiptAnchor | `anchor_batch` requires the merchant's auth | `anchor_batch` | `receipt-anchor/src/test.rs` -> `test_anchor_batch_requires_merchant_auth` |

## `Governance`

| Invariant | Enforcement path | Test reference |
|---|---|---|
| Membership is bounded and well-formed: 1..=32 members, no duplicates, no zero weights, weights sum without overflow; `threshold_bps` in `1..=10_000`; voting period non-zero | `__constructor` (`MAX_MEMBERS`, `MIN_/MAX_THRESHOLD_BPS`) | `governance/src/test.rs` -> `constructor_rejects_mismatched_lengths`, `constructor_rejects_zero_threshold`, `constructor_rejects_zero_voting_period` |
| Only members propose and vote | `propose`, `vote` (`NotAMember`) | `governance/src/test.rs` -> `non_member_cannot_propose` |
| One vote per member per proposal | `vote` (`AlreadyVoted`) | `governance/src/test.rs` -> `cannot_vote_twice` |
| Votes after the deadline ledger are rejected | `vote` (`VotingClosed`) | `governance/src/test.rs` -> `voting_closes_after_deadline` |
| A member's weight is the integer square root of its deposit (quadratic weighting), so a large deposit cannot dominate linearly | `voting` / `math` | `governance/src/test.rs` -> `quadratic_weight_is_sqrt_of_deposit`, `quadratic_weight_prevents_whale_domination` |
| Effective quorum decays linearly from `threshold_bps` toward the absolute floor `QUORUM_FLOOR_BPS` (3,500 bps) and never below it | `quorum::current_quorum_bps` | `governance/src/test.rs` -> `quorum_decays_linearly_over_the_voting_window`, `quorum_floor_still_requires_real_opposition_is_outweighed` (see `quorum_at_midpoint_is_lower_than_initial_but_above_floor`) |
| Execution requires "yes" weight to clear the effective quorum **and** strictly exceed "no" weight (`QuorumNotMet`) | `execute` | `governance/src/test.rs` -> `quorum_below_threshold_blocks_execution`, `no_votes_can_outweigh_a_stale_quorum` |
| A proposal executes at most once (`AlreadyExecuted`) | `execute` | `governance/src/test.rs` -> `cannot_execute_twice` |
| Active proposals cannot be pruned; expired or executed ones can | `prune_proposal` (`ProposalActive`) | `governance/src/test.rs` -> `prune_rejects_active_proposal_then_succeeds_once_expired` |
| An anonymous (LSAG) vote reveals no signer: a key image cannot be reused on the same proposal, an invalid signature changes nothing, and a member who voted transparently cannot enter an anonymous ring | `vote_anonymous`, `ring_sig` | `governance/src/test.rs` -> `duplicate_key_image_is_rejected_without_a_second_tally_or_event`, `invalid_anonymous_signature_leaves_tally_storage_and_events_unchanged`, `prior_transparent_vote_prevents_that_member_from_entering_an_anonymous_ring` |
| Optimistic proposals are vetoable only inside the challenge window and do not execute once the supermajority veto is reached | `optimistic` | `governance/src/test.rs` -> `execute_optimistic_fails_when_supermajority_vetoes`, `optimistic_submit_and_execute_after_challenge_window` |

## `MultisigAccount`

| Invariant | Enforcement path | Test reference |
|---|---|---|
| An authorization is valid only if every attached delegated signer is registered and the combined weight of distinct delegates reaches `threshold` | `__check_auth` | `multisig-account/src/test.rs` -> `pause_by_unregistered_delegates_is_rejected`, `pause_below_threshold_is_rejected` |
| Signer-set changes (add/remove/weight/rotate) require the account's own threshold auth | `signers`, `admin` | `multisig-account/src/test.rs` -> `set_guardian_requires_threshold`, `signer_rotation_works_while_paused` |
| While paused, outbound contexts are rejected but recovery contexts (rotation, unpause) are still allowed | `__check_auth` pause gate | `multisig-account/src/test.rs` -> `check_auth_returns_paused_for_outbound_contexts`, `check_auth_allows_recovery_contexts_while_paused`, `dispatch_is_blocked_while_paused` |
| A single signer cannot pause or unpause alone; only threshold signers or the registered guardian can, and a cleared guardian loses that power | `pause` / `unpause` | `multisig-account/src/test.rs` -> `signer_cannot_pause_alone_by_naming_itself`, `guardian_can_pause_and_unpause`, `cleared_guardian_loses_pause_authority` |
| Read-only queries keep working while paused | views | `multisig-account/src/test.rs` -> `read_only_queries_work_while_paused` |

## `UptoAuthorization`

| Invariant | Enforcement path | Test reference |
|---|---|---|
| A payment is settled at most once per `payment_id` | `settle` | `upto-authorization/src/test.rs` -> `test_single_settlement_second_settle_fails` |
| Settled amount never exceeds `cap` plus the authorised slippage (`cap * (10_000 + max_slippage_bps) / 10_000`, rounded down); zero bps is a strict cap | `settle` | `upto-authorization/src/test.rs` -> `test_settle_exceeding_cap_fails`, `test_settle_at_slippage_boundary`, `test_zero_bps_is_strict_cap` |
| Settlement is rejected after `expiry` and accepted through the expiry ledger | `settle` | `upto-authorization/src/test.rs` -> `test_settle_after_expiry_fails`, `test_settle_at_expiry_boundary_succeeds` |
| The recipient is bound at authorization and cannot be redirected at settle | `settle` | `upto-authorization/src/test.rs` -> `test_settle_cannot_redirect_to_different_recipient` |
| The contract holds no funds and leaves no residual allowance after settlement | `settle` (SEP-41 allowance model) | `upto-authorization/src/test.rs` -> `test_contract_holds_no_funds`, `test_no_residual_allowance_after_settlement`, `test_cap_minus_actual_does_not_linger` |
| Signed authorizations bind every field, the network, and a registered signer | `domain`, `authorize_signed` | `upto-authorization/src/test.rs` -> `test_authorization_digest_binds_every_field`, `test_authorize_signed_rejected_on_different_network`, `test_authorize_signed_unregistered_signer_fails` |
| Only lapsed authorizations can be pruned | `prune_*` | `upto-authorization/src/test.rs` -> `test_prune_active_authorization_fails`, `test_prune_expired_authorization_succeeds` |
| Only the buyer can cancel a dormant authorization, only after the inactivity window, and never a settled one | `cancel_inactive_escrow` | `upto-authorization/src/inactivity_test.rs` -> `cancelling_at_the_edge_of_the_window_is_rejected`, `the_buyer_must_authorize_the_cancellation`, `a_settled_authorization_cannot_be_cancelled` |

## `CrossChain`

| Invariant | Enforcement path | Test reference |
|---|---|---|
| Inbound LayerZero packets are accepted only from the configured endpoint and a trusted peer, on the expected chain | `layerzero` | `cross-chain/src/layerzero_test.rs` -> `untrusted_peer_is_rejected`, `wrong_chain_is_rejected_even_for_a_trusted_peer`, `unconfigured_endpoint_fails_closed` |
| Packet nonces strictly advance per peer channel; replayed and stale nonces are rejected | `layerzero` | `cross-chain/src/layerzero_test.rs` -> `replayed_and_stale_nonces_are_rejected`, `later_nonces_are_accepted_and_advance_the_channel` |
| A dispute is settled at most once | `layerzero` | `cross-chain/src/layerzero_test.rs` -> `settled_dispute_cannot_be_settled_again` |
| An Axelar message id is credited at most once, from a trusted gateway | `axelar` | `cross-chain/src/axelar_test.rs` -> `replayed_message_is_rejected`, `untrusted_gateway_is_rejected` |
| A Wormhole VAA needs a known guardian set, ordered valid signatures and quorum | `wormhole` | `cross-chain/src/wormhole_test.rs` -> `test_verify_vaa_insufficient_quorum_rejected`, `test_verify_vaa_out_of_order_guardian_index_rejected` |
| Outbound withdrawals burn the wrapped asset before emitting the bridge event, reject non-positive amounts, and are blocked while paused | `outbound`, `lib` | `cross-chain/src/test.rs` -> `test_withdraw_to_evm_burns_asset_and_emits_event`, `test_withdraw_invalid_amount_rejected`, `test_withdraw_when_paused_rejected` |
| Configuration setters are admin-only | `lib` | `cross-chain/src/test.rs` -> `admin_gated_configuration_rejects_non_admin` |

## `StreamVault`

| Invariant | Enforcement path | Test reference |
|---|---|---|
| The contract's token balance equals the unclaimed principal of its open streams (buyer escrow is separate from merchant float) | all entry points | `stream-vault/src/test.rs` -> `balance_equals_unclaimed_principal_of_open_streams` |
| Streamed amount is `min(deposit, elapsed * rate_per_ledger)`, never exceeds the deposit, and is zero at or before `start_ledger` | `streamed_amount` | `stream-vault/src/test.rs` -> `claim_long_after_stop_pays_only_the_deposit`, `nothing_claimable_before_or_at_start_ledger`, `fractional_final_ledger_is_clamped_to_deposit` |
| `claim_stream` is permissionless but only ever pays the merchant | `claim_stream` | `stream-vault/src/test.rs` -> `claim_is_permissionless` |
| Pausing freezes accrual; resuming shifts the schedule so paused ledgers are not charged; only the buyer can pause or resume | `pause_stream`, `resume_stream` | `stream-vault/src/test.rs` -> `pause_freezes_accrual_and_resume_shifts_schedule`, `pause_and_resume_require_buyer_auth` |
| Cancel splits the deposit between merchant and buyer with no loss or creation of funds | `cancel_stream` | `stream-vault/src/test.rs` -> `cancel_fractional_split_conserves_deposit`, `test_mid_term_cancel_split_matches_streamed_amount_and_conserves_deposit` |
| A stream closes once fully claimed; invalid parameters and the contract as buyer are rejected | `create_stream`, `claim_stream` | `stream-vault/src/test.rs` -> `claim_at_stop_ledger_closes_stream`, `create_stream_rejects_invalid_params`, `create_stream_rejects_contract_as_buyer` |

## `Treasury`

| Invariant | Enforcement path | Test reference |
|---|---|---|
| Initialization is single-shot; configuration and portfolio actions are admin-only | `initialize`, admin setters | `treasury/src/test.rs` -> `double_initialization_is_rejected`, `only_the_admin_can_create_schedules`, `only_the_admin_can_touch_the_portfolio` |
| A vesting schedule never pays out more than its total, and nothing is claimable before the cliff | `vesting`, `claim_vested` | `treasury/src/test.rs` -> `one_schedule_never_pays_out_more_than_its_total`, `nothing_can_be_claimed_before_the_cliff` |
| A beneficiary has at most one schedule; degenerate schedules are rejected | `add_schedule` | `treasury/src/test.rs` -> `a_beneficiary_cannot_have_two_schedules`, `invalid_schedules_are_rejected` |
| Only whitelisted strategies receive allocations; allocations sum to 100%; a strategy holding funds cannot be revoked | `strategies` | `treasury/src/strategies_test.rs` -> `only_whitelisted_strategies_can_be_allocated`, `allocations_must_sum_to_one_hundred_percent`, `a_strategy_holding_funds_cannot_be_revoked` |
| Recalls cannot exceed deployed principal | `recall_strategy` | `treasury/src/strategies_test.rs` -> `recalls_beyond_the_deployed_principal_are_rejected` |
| Liquidation and buyback fail closed when unconfigured and enforce the oracle/slippage floor | `liquidation`, `buyback` | `treasury/src/liquidation_test.rs` -> `liquidation_before_configuration_fails_closed`, `a_swap_below_the_oracle_floor_is_rejected`; `treasury/src/buyback_test.rs` -> `buyback_reverts_when_slippage_floor_is_not_met` |
| The yield accumulator is monotonically increasing and a checkpoint prevents double-claiming | `distribution` | `treasury/src/distribution_test.rs` -> `accumulator_is_monotonically_increasing`, `checkpoint_prevents_double_claiming` |
