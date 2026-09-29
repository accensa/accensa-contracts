//! Unit tests for the storage eviction policy (issue #395).

#![cfg(test)]

use crate::pruning::{PRUNE_BOUNTY_PER_BATCH, RETENTION_LEDGERS};
use crate::{Error, ReceiptShard, ReceiptShardClient};
use soroban_sdk::{
    testutils::{Address as _, Events as _, Ledger},
    token::StellarAssetClient,
    vec, Address, BytesN, Env, IntoVal, Map, Symbol, Val,
};

/// Base ledger sequence the fixtures anchor at. Arbitrary but comfortably
/// above 0 so tests can advance to `BASE + RETENTION_LEDGERS + n` without
/// u32 overflow concerns (3.1M + 100K << u32::MAX).
const BASE_LEDGER: u32 = 100_000;

/// Shard fixture with a router and range `[1, 1001)`.
struct Fixture {
    env: Env,
    client: ReceiptShardClient<'static>,
    #[allow(dead_code)]
    router: Address,
    pruner: Address,
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let pruner = Address::generate(&env);
    let contract_id = env.register(ReceiptShard, (router.clone(), 1u64, 1001u64));
    let client = ReceiptShardClient::new(&env, &contract_id);
    env.ledger().with_mut(|li| li.sequence_number = BASE_LEDGER);
    Fixture {
        env,
        client,
        router,
        pruner,
    }
}

/// Anchor `count` batches starting at `start` at the *current* ledger.
fn anchor_now(fx: &Fixture, start: u64, count: u64) {
    let root = BytesN::from_array(&fx.env, &[7u8; 32]);
    let seq = fx.env.ledger().sequence();
    for i in 0..count {
        fx.env.ledger().with_mut(|li| li.sequence_number = seq);
        fx.client.anchor_batch(&(start + i), &root, &1, &0, &100);
    }
}

/// Advance the ledger `n` ledgers past the current one.
fn advance(fx: &Fixture, n: u32) {
    let seq = fx.env.ledger().sequence();
    fx.env.ledger().with_mut(|li| li.sequence_number = seq + n);
}

// ── Retention policy ─────────────────────────────────────────────────────────

#[test]
fn active_unexpired_receipts_are_never_pruned() {
    let fx = setup();
    anchor_now(&fx, 1, 5);

    // Even a huge max_count deletes nothing: every batch is within retention.
    assert_eq!(fx.client.prune_expired_receipts(&fx.pruner, &u32::MAX), 0);

    // And nothing was actually deleted.
    for id in 1..=5u64 {
        assert!(fx.client.try_get_batch(&id).is_ok());
    }
}

#[test]
fn expired_receipts_are_pruned() {
    let fx = setup();
    anchor_now(&fx, 1, 3);
    // Age the batches just past the retention window.
    advance(&fx, RETENTION_LEDGERS + 10);

    assert_eq!(fx.client.prune_expired_receipts(&fx.pruner, &10), 3);

    for id in 1..=3u64 {
        assert_eq!(fx.client.try_get_batch(&id), Err(Ok(Error::BatchNotFound)));
    }
    // The shard's assigned range is untouched by pruning.
    assert_eq!(fx.client.get_range(), (1, 1001));
}

#[test]
fn expiry_deletions_ahead_of_cursor_are_consumed_when_prefix_catches_up() {
    let env = Env::default();
    env.mock_all_auths();
    let router = Address::generate(&env);
    let pruner = Address::generate(&env);
    let id = env.register(ReceiptShard, (router, 1u64, 5u64));
    let client = ReceiptShardClient::new(&env, &id);
    env.ledger()
        .with_mut(|ledger| ledger.sequence_number = BASE_LEDGER);

    // Batch 2 expires while batch 1 is still retained, so policy pruning
    // deletes it ahead of the contiguous cursor and records a tombstone.
    client.anchor_batch(&2, &BytesN::from_array(&env, &[7u8; 32]), &1, &0, &100);
    env.ledger().with_mut(|ledger| {
        ledger.sequence_number += RETENTION_LEDGERS + 1;
    });
    client.anchor_batch(&1, &BytesN::from_array(&env, &[8u8; 32]), &1, &0, &100);

    assert_eq!(client.prune_expired_receipts(&pruner, &10), 1);
    assert_eq!(client.get_shard_diagnostics().oldest_unpruned_batch_id, 1);

    // Once batch 1 expires too, advancing across it consumes the known-deleted
    // batch 2 marker instead of leaving PrunedUpTo stuck at 2.
    env.ledger().with_mut(|ledger| {
        ledger.sequence_number += RETENTION_LEDGERS + 1;
    });
    assert_eq!(client.prune_expired_receipts(&pruner, &10), 1);
    assert_eq!(client.get_shard_diagnostics().oldest_unpruned_batch_id, 3);
}

#[test]
fn boundary_batch_exactly_at_retention_is_prunable() {
    let fx = setup();
    anchor_now(&fx, 1, 1);
    // `current - anchored == RETENTION_LEDGERS` is prunable (>=).
    advance(&fx, RETENTION_LEDGERS);

    assert_eq!(fx.client.prune_expired_receipts(&fx.pruner, &1), 1);
}

#[test]
fn one_ledger_short_of_retention_is_kept() {
    let fx = setup();
    anchor_now(&fx, 1, 1);
    // Age is exactly one ledger short of the window.
    advance(&fx, RETENTION_LEDGERS - 1);

    assert_eq!(fx.client.prune_expired_receipts(&fx.pruner, &1), 0);
    assert!(fx.client.try_get_batch(&1).is_ok());
}

// ── max_count and cursor semantics ───────────────────────────────────────────

#[test]
fn max_count_limits_deletions_per_call() {
    let fx = setup();
    anchor_now(&fx, 1, 5);
    advance(&fx, RETENTION_LEDGERS + 1);

    assert_eq!(fx.client.prune_expired_receipts(&fx.pruner, &2), 2);
    // A second call continues from where the first stopped.
    assert_eq!(fx.client.prune_expired_receipts(&fx.pruner, &2), 2);
    assert_eq!(fx.client.prune_expired_receipts(&fx.pruner, &10), 1);
}

// ── Bounty accounting ────────────────────────────────────────────────────────

#[test]
fn bounty_accrues_per_pruned_batch_and_is_attributed_to_caller() {
    let fx = setup();
    anchor_now(&fx, 1, 4);
    advance(&fx, RETENTION_LEDGERS + 1);

    fx.client.prune_expired_receipts(&fx.pruner, &3);
    assert_eq!(
        fx.client.get_prune_bounty(&fx.pruner),
        3 * PRUNE_BOUNTY_PER_BATCH
    );

    // A different caller has nothing accrued.
    let other = Address::generate(&fx.env);
    assert_eq!(fx.client.get_prune_bounty(&other), 0);
}

#[test]
fn no_pruning_accrues_no_bounty() {
    let fx = setup();
    anchor_now(&fx, 1, 2);

    fx.client.prune_expired_receipts(&fx.pruner, &5);
    assert_eq!(fx.client.get_prune_bounty(&fx.pruner), 0);
}

// ── Claim payout ─────────────────────────────────────────────────────────────

#[test]
fn claim_pays_accrued_bounty_from_contract_balance() {
    let fx = setup();
    anchor_now(&fx, 1, 3);
    advance(&fx, RETENTION_LEDGERS + 1);

    fx.client.prune_expired_receipts(&fx.pruner, &3);
    let accrued = 3 * PRUNE_BOUNTY_PER_BATCH;

    // Fund the contract so it can pay the bounty.
    let token_admin = Address::generate(&fx.env);
    let sac = fx.env.register_stellar_asset_contract_v2(token_admin);
    let token_address = sac.address();
    StellarAssetClient::new(&fx.env, &token_address).mint(&fx.client.address, &accrued);

    let paid = fx.client.claim_prune_bounty(&token_address, &fx.pruner);
    assert_eq!(paid, accrued);
    assert_eq!(fx.client.get_prune_bounty(&fx.pruner), 0);

    // Double-claim pays nothing.
    let paid_again = fx.client.claim_prune_bounty(&token_address, &fx.pruner);
    assert_eq!(paid_again, 0);
}

#[test]
fn claim_is_capped_at_contract_balance() {
    let fx = setup();
    anchor_now(&fx, 1, 2);
    advance(&fx, RETENTION_LEDGERS + 1);

    fx.client.prune_expired_receipts(&fx.pruner, &2);
    let accrued = 2 * PRUNE_BOUNTY_PER_BATCH;

    let token_admin = Address::generate(&fx.env);
    let sac = fx.env.register_stellar_asset_contract_v2(token_admin);
    let token_address = sac.address();
    // Contract holds less than the accrued bounty.
    StellarAssetClient::new(&fx.env, &token_address).mint(&fx.client.address, &(accrued - 1));

    let paid = fx.client.claim_prune_bounty(&token_address, &fx.pruner);
    assert_eq!(paid, accrued - 1);
    // The accrual was still fully consumed — the shortfall is not deferred.
    assert_eq!(fx.client.get_prune_bounty(&fx.pruner), 0);
}

// ── Events ───────────────────────────────────────────────────────────────────

#[test]
fn receipts_pruned_event_emitted_with_count() {
    let fx = setup();
    anchor_now(&fx, 1, 3);
    advance(&fx, RETENTION_LEDGERS + 1);

    fx.client.prune_expired_receipts(&fx.pruner, &2);

    let mut data = Map::<Symbol, Val>::new(&fx.env);
    data.set(Symbol::new(&fx.env, "new_cursor"), 3u64.into_val(&fx.env));
    data.set(Symbol::new(&fx.env, "pruned_count"), 2u32.into_val(&fx.env));
    assert_eq!(
        fx.env.events().all().filter_by_contract(&fx.client.address),
        vec![
            &fx.env,
            (
                fx.client.address.clone(),
                (
                    Symbol::new(&fx.env, "receipts_pruned_event"),
                    fx.pruner.clone()
                )
                    .into_val(&fx.env),
                data.into_val(&fx.env)
            )
        ]
    );
}

#[test]
fn receipts_pruned_event_emitted_on_noop_with_zero() {
    let fx = setup();
    anchor_now(&fx, 1, 1);

    fx.client.prune_expired_receipts(&fx.pruner, &5);

    let mut data = Map::<Symbol, Val>::new(&fx.env);
    data.set(Symbol::new(&fx.env, "new_cursor"), 1u64.into_val(&fx.env));
    data.set(Symbol::new(&fx.env, "pruned_count"), 0u32.into_val(&fx.env));
    assert_eq!(
        fx.env.events().all().filter_by_contract(&fx.client.address),
        vec![
            &fx.env,
            (
                fx.client.address.clone(),
                (
                    Symbol::new(&fx.env, "receipts_pruned_event"),
                    fx.pruner.clone()
                )
                    .into_val(&fx.env),
                data.into_val(&fx.env)
            )
        ]
    );
}

// ── Config accessors ─────────────────────────────────────────────────────────

#[test]
fn policy_constants_are_readable() {
    let fx = setup();
    assert_eq!(fx.client.get_retention_ledgers(), RETENTION_LEDGERS);
    assert_eq!(
        fx.client.get_prune_bounty_per_batch(),
        PRUNE_BOUNTY_PER_BATCH
    );
}

// ── Router cursor pruning coexistence ────────────────────────────────────────

#[test]
fn router_cursor_pruning_and_policy_pruning_coexist() {
    let fx = setup();
    anchor_now(&fx, 1, 4);
    advance(&fx, RETENTION_LEDGERS + 1);

    // Router prunes the first two by cursor (before_ledger = now prunes all
    // anchored before it).
    let seq = fx.env.ledger().sequence();
    let (cursor, pruned) = fx.client.prune_batches(&seq, &2, &100);
    assert_eq!((cursor, pruned), (3, 2));

    // The policy path starts from the advanced cursor; the remaining two
    // batches are also expired, so they go too.
    assert_eq!(fx.client.prune_expired_receipts(&fx.pruner, &10), 2);
    assert_eq!(
        fx.client.get_prune_bounty(&fx.pruner),
        2 * PRUNE_BOUNTY_PER_BATCH
    );
}
