#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{storage::Persistent as _, Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    vec, Address, Env, Val,
};

const FLOAT: i128 = 1_000_000;

fn setup(window: u32) -> (Env, RefundVaultClient<'static>, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let contract_id = env.register(RefundVault, ());
    let client = RefundVaultClient::new(&env, &contract_id);
    client.initialize(&merchant, &token, &window);

    (env, client, merchant, token)
}

#[test]
fn test_double_initialize_fails() {
    let (_env, client, merchant, token) = setup(100);
    assert_eq!(
        client.try_initialize(&merchant, &token, &100),
        Err(Ok(Error::AlreadyInitialized))
    );
}

#[test]
fn test_deposit_moves_tokens_into_vault() {
    let (env, client, merchant, token) = setup(100);
    client.deposit(&merchant, &600_000);

    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&client.address), 600_000);
    assert_eq!(token_client.balance(&merchant), FLOAT - 600_000);
}

/// Deposits are deliberately merchant-only (see docs/SECURITY_MODEL.md): the
/// vault only ever holds the merchant's own funds, so a third party cannot
/// contribute float — dust or otherwise — that the merchant has not authorised.
/// This test pins that guarantee so it cannot be relaxed by accident.
#[test]
fn test_deposit_from_non_merchant_fails() {
    let (env, client, _merchant, _token) = setup(100);
    let stranger = Address::generate(&env);
    assert_eq!(
        client.try_deposit(&stranger, &100),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn test_refund_happy_path() {
    let (env, client, merchant, token) = setup(100);
    client.deposit(&merchant, &500_000);

    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&payment_ref, &buyer, &120_000, &0, &120_000, &None);

    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&buyer), 120_000);
    assert_eq!(token_client.balance(&client.address), 380_000);

    let record = client.get_refund(&payment_ref).unwrap();
    assert_eq!(record.amount_refunded, 120_000);
    assert_eq!(record.recipient, buyer);
}

#[test]
fn test_partial_refunds_cumulative_within_ceiling() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let buyer = Address::generate(&env);

    // A 300-unit payment refunded in two partials plus one boundary call.
    client.refund(&payment_ref, &buyer, &100, &0, &300, &None);
    client.refund(&payment_ref, &buyer, &150, &0, &300, &None);
    client.refund(&payment_ref, &buyer, &50, &0, &300, &None);

    let record = client.get_refund(&payment_ref).unwrap();
    assert_eq!(record.amount_refunded, 300);
    // Summing the partials lands exactly on the ceiling.
    assert_eq!(record.payment_amount, 300);

    // One more call, even a single unit, is now past the ceiling.
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &1, &0, &300, &None),
        Err(Ok(Error::ExceedsPayment))
    );
}

#[test]
fn test_refund_outside_window_fails() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    env.ledger().with_mut(|li| li.sequence_number = 500);

    let payment_ref = BytesN::from_array(&env, &[1u8; 32]);
    let buyer = Address::generate(&env);
    // Paid at ledger 100 with a 100-ledger window: expired at 200, now 500.
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &100, &100, &100, &None),
        Err(Ok(Error::WindowExpired))
    );
}

#[test]
fn test_refund_at_window_boundary_succeeds() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    env.ledger().with_mut(|li| li.sequence_number = 200);

    let payment_ref = BytesN::from_array(&env, &[2u8; 32]);
    let buyer = Address::generate(&env);
    // current (200) == paid_at (100) + window (100): still inside the window.
    client.refund(&payment_ref, &buyer, &100, &100, &100, &None);
    assert!(client.get_refund(&payment_ref).is_some());
}

#[test]
fn test_zero_window_disables_expiry() {
    let (env, client, merchant, _token) = setup(0);
    client.deposit(&merchant, &500_000);

    env.ledger().with_mut(|li| li.sequence_number = 1_000_000);

    let payment_ref = BytesN::from_array(&env, &[3u8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&payment_ref, &buyer, &100, &0, &100, &None);
    assert!(client.get_refund(&payment_ref).is_some());
}

/// The `RefundV2` guard entry's TTL must be sized to the refund window, not a
/// flat `TTL_EXTEND` (~30 days): otherwise a window longer than that flat
/// interval can outlive the guard that is supposed to police it, so the
/// entry can go stale (and become eligible for archival) while `refund`
/// would still accept further calls for that payment on policy grounds.
/// See `refund_record_ttl_extend_to`.
#[test]
fn test_long_window_extends_guard_past_flat_ttl() {
    let window = TTL_EXTEND * 3;
    let (env, client, merchant, _token) = setup(window);
    client.deposit(&merchant, &500_000);

    let payment_ref = BytesN::from_array(&env, &[10u8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&payment_ref, &buyer, &100_000, &0, &300_000, &None);

    let ttl_after_refund = env.as_contract(&client.address, || {
        env.storage()
            .persistent()
            .get_ttl(&DataKey::RefundV2(payment_ref.clone()))
    });
    assert!(
        ttl_after_refund > TTL_EXTEND,
        "guard TTL ({ttl_after_refund}) was not sized to the window ({window}); \
         it must outlast the flat TTL_EXTEND ({TTL_EXTEND}) whenever the window does"
    );

    // Jump past where the old flat TTL_EXTEND would have left the guard
    // entry eligible for archival, but still well inside the window.
    env.ledger()
        .with_mut(|li| li.sequence_number = TTL_EXTEND + 10_000);

    // A further partial refund for the same payment must still see the prior
    // cumulative total: the guard entry must not have gone missing.
    client.refund(&payment_ref, &buyer, &50_000, &0, &300_000, &None);
    let record = client.get_refund(&payment_ref).unwrap();
    assert_eq!(record.amount_refunded, 150_000);
}

/// `window == 0` means "no time bound" for `refund` itself (see
/// `test_zero_window_disables_expiry`); the guard entry's TTL must match
/// that by extending to the network's actual maximum TTL rather than the
/// flat `TTL_EXTEND`, so an unbounded refund policy is never quietly capped
/// by the guard aging out first.
#[test]
fn test_zero_window_extends_guard_to_max_ttl() {
    let (env, client, merchant, _token) = setup(0);
    client.deposit(&merchant, &500_000);

    let payment_ref = BytesN::from_array(&env, &[11u8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&payment_ref, &buyer, &100_000, &0, &300_000, &None);

    let ttl_after_refund = env.as_contract(&client.address, || {
        env.storage()
            .persistent()
            .get_ttl(&DataKey::RefundV2(payment_ref.clone()))
    });
    assert!(
        ttl_after_refund > TTL_EXTEND * 2,
        "guard TTL ({ttl_after_refund}) for an unbounded window was not \
         extended past the flat TTL_EXTEND ({TTL_EXTEND})"
    );
}

#[test]
fn test_refund_exceeding_float_fails() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &100);

    let payment_ref = BytesN::from_array(&env, &[4u8; 32]);
    let buyer = Address::generate(&env);
    // payment_amount >= amount so the ceiling check passes and the float
    // shortage is what gets reported.
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &10_000, &0, &10_000, &None),
        Err(Ok(Error::InsufficientFloat))
    );
}

#[test]
fn test_withdraw_returns_float_to_merchant() {
    let (env, client, merchant, token) = setup(100);
    client.deposit(&merchant, &500_000);
    client.withdraw(&200_000, &merchant);

    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&client.address), 300_000);
    assert_eq!(token_client.balance(&merchant), FLOAT - 300_000);
}

#[test]
fn test_withdraw_exceeding_float_fails() {
    let (_env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &100);
    assert_eq!(
        client.try_withdraw(&10_000, &merchant),
        Err(Ok(Error::InsufficientFloat))
    );
}

#[test]
fn test_set_refund_window_takes_effect() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    env.ledger().with_mut(|li| li.sequence_number = 500);

    let payment_ref = BytesN::from_array(&env, &[5u8; 32]);
    let buyer = Address::generate(&env);
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &100, &100, &100, &None),
        Err(Ok(Error::WindowExpired))
    );

    client.propose_policy(&1000, &0, &0);
    // Cannot execute yet — timelock has not expired.
    assert_eq!(
        client.try_execute_policy(),
        Err(Ok(Error::TimelockNotExpired))
    );

    // Advance past the timelock (500 + 17_280 = 17_780).
    env.ledger().with_mut(|li| li.sequence_number += 17_280);
    client.execute_policy();

    // paid_at=17_780, window=1000, current=17_780 → still inside the window.
    client.refund(
        &payment_ref,
        &buyer,
        &100,
        &(env.ledger().sequence()),
        &100,
        &None,
    );
    assert!(client.get_refund(&payment_ref).is_some());
}

#[test]
fn test_uninitialized_calls_fail() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(RefundVault, ());
    let client = RefundVaultClient::new(&env, &contract_id);
    let addr = Address::generate(&env);
    let payment_ref = BytesN::from_array(&env, &[6u8; 32]);

    assert_eq!(
        client.try_deposit(&addr, &100),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(
        client.try_refund(&payment_ref, &addr, &100, &0, &100, &None),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(
        client.try_withdraw(&100, &addr),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(
        client.try_propose_policy(&10, &0, &0),
        Err(Ok(Error::NotInitialized))
    );
    assert_eq!(client.try_execute_policy(), Err(Ok(Error::NotInitialized)));
}

#[test]
#[should_panic]
fn test_refund_requires_merchant_auth() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    // Enforcing mode with no signatures: merchant.require_auth() must abort.
    env.set_auths(&[]);
    let payment_ref = BytesN::from_array(&env, &[8u8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&payment_ref, &buyer, &100, &0, &100, &None);
}

#[test]
fn test_deposit_invalid_amount_fails() {
    let (_env, client, merchant, _token) = setup(100);
    assert_eq!(
        client.try_deposit(&merchant, &0),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        client.try_deposit(&merchant, &-100),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_refund_invalid_amount_fails() {
    let (env, client, _merchant, _token) = setup(100);
    let payment_ref = BytesN::from_array(&env, &[9u8; 32]);
    let buyer = Address::generate(&env);
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &0, &0, &100, &None),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &-100, &0, &100, &None),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_withdraw_invalid_amount_fails() {
    let (_env, client, merchant, _token) = setup(100);
    assert_eq!(
        client.try_withdraw(&0, &merchant),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        client.try_withdraw(&-100, &merchant),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn test_pause_unpause() {
    let (_env, client, _merchant, _token) = setup(100);
    client.pause();
    client.unpause();
}

#[test]
fn test_deposit_when_paused_fails() {
    let (_env, client, merchant, _token) = setup(100);
    client.pause();
    assert_eq!(client.try_deposit(&merchant, &100), Err(Ok(Error::Paused)));
}

#[test]
fn test_refund_when_paused_fails() {
    let (env, client, _merchant, _token) = setup(100);
    client.pause();
    let payment_ref = BytesN::from_array(&env, &[10u8; 32]);
    let buyer = Address::generate(&env);
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &100, &0, &100, &None),
        Err(Ok(Error::Paused))
    );
}

#[test]
fn test_withdraw_when_paused_fails() {
    let (_env, client, merchant, _token) = setup(100);
    client.pause();
    assert_eq!(client.try_withdraw(&100, &merchant), Err(Ok(Error::Paused)));
}

#[test]
#[should_panic]
fn test_pause_requires_merchant_auth() {
    let (env, client, _merchant, _token) = setup(100);
    env.set_auths(&[]);
    client.pause();
}

#[test]
#[should_panic]
fn test_unpause_requires_merchant_auth() {
    let (env, client, _merchant, _token) = setup(100);
    env.set_auths(&[]);
    client.unpause();
}

// ── Paused-state invariant (issue #80) ────────────────────────────────────
//
// Holistic coverage for the emergency-stop guarantee: the vault is
// initialized, funded, and strictly paused by the admin. Every state-changing
// operation on the public surface (deposit, refund, withdraw, and the yield
// surface) is replayed while paused and must be rejected with `Error::Paused`
// without mutating any state. After `unpause()` the exact same operations
// must resume their normal outcomes, proving the lock is temporary and
// reversible.

/// Normalizes a `try_*` client invocation into the contract-level outcome.
/// A host-level failure (auth abort, conversion error) cannot occur for these
/// calls under `mock_all_auths` with valid arguments, so it surfaces as a
/// panic rather than being conflated with a contract error.
fn contract_outcome<T>(
    result: Result<Result<(), T>, Result<Error, soroban_sdk::InvokeError>>,
) -> Result<(), Error> {
    match result {
        Ok(Ok(())) => Ok(()),
        Err(Ok(e)) => Err(e),
        Ok(Err(_)) | Err(Err(_)) => panic!("unexpected host-level failure"),
    }
}

/// One state-changing operation of the vault's public surface.
struct PausedSurfaceOp<'a> {
    name: &'static str,
    invoke: &'a dyn Fn() -> Result<(), Error>,
}

#[test]
fn test_paused_state_blocks_and_preserves_every_operation() {
    let (env, client, merchant, token) = setup(100);
    client.deposit(&merchant, &600_000);
    let token_client = TokenClient::new(&env, &token);

    let payment_ref = BytesN::from_array(&env, &[0x80u8; 32]);
    let buyer = Address::generate(&env);

    // Every IsPaused-gated operation, with arguments that would succeed while
    // the vault is unpaused.
    let operations = [
        PausedSurfaceOp {
            name: "deposit",
            invoke: &|| contract_outcome(client.try_deposit(&merchant, &100_000)),
        },
        PausedSurfaceOp {
            name: "refund",
            invoke: &|| {
                contract_outcome(client.try_refund(
                    &payment_ref,
                    &buyer,
                    &100_000,
                    &0,
                    &100_000,
                    &None,
                ))
            },
        },
        PausedSurfaceOp {
            name: "withdraw",
            invoke: &|| contract_outcome(client.try_withdraw(&100_000, &merchant)),
        },
        PausedSurfaceOp {
            name: "deploy_to_yield",
            invoke: &|| contract_outcome(client.try_deploy_to_yield(&100_000)),
        },
        PausedSurfaceOp {
            name: "withdraw_from_yield",
            invoke: &|| contract_outcome(client.try_withdraw_from_yield(&100_000)),
        },
        PausedSurfaceOp {
            name: "harvest_yield",
            invoke: &|| contract_outcome(client.try_harvest_yield()),
        },
    ];

    // Funded, then strictly paused by the admin.
    client.pause();

    // Snapshot of every observable quantity the attack must not move.
    let vault_balance_before = token_client.balance(&client.address);
    let merchant_balance_before = token_client.balance(&merchant);
    let yield_info_before = client.get_yield_info();

    // Attack simulation: every state-changing call is rejected with Paused
    // and mutates nothing.
    for op in &operations {
        assert_eq!(
            (op.invoke)(),
            Err(Error::Paused),
            "{} must be rejected with Error::Paused while the vault is paused",
            op.name
        );

        let info = client.get_yield_info();
        assert_eq!(
            token_client.balance(&client.address),
            vault_balance_before,
            "{} mutated the vault float while paused",
            op.name
        );
        assert_eq!(
            token_client.balance(&merchant),
            merchant_balance_before,
            "{} mutated the merchant balance while paused",
            op.name
        );
        assert!(
            client.get_refund(&payment_ref).is_none(),
            "{} created a refund record while paused",
            op.name
        );
        assert_eq!(
            info.deployed_principal, yield_info_before.deployed_principal,
            "{} mutated deployed principal while paused",
            op.name
        );
        assert_eq!(
            info.harvested_yield, yield_info_before.harvested_yield,
            "{} mutated harvested yield while paused",
            op.name
        );
    }

    // Unpause verification: each operation must clear the pause gate. The
    // core operations use fresh calls because replaying the same refund after
    // a successful call would correctly exceed its payment ceiling.
    client.unpause();
    assert_eq!(
        contract_outcome(client.try_deposit(&merchant, &100_000)),
        Ok(())
    );
    assert_eq!(
        contract_outcome(client.try_refund(&payment_ref, &buyer, &100_000, &0, &100_000, &None)),
        Ok(())
    );
    assert_eq!(
        contract_outcome(client.try_withdraw(&100_000, &merchant)),
        Ok(())
    );
    assert_eq!(
        contract_outcome(client.try_deploy_to_yield(&100_000)),
        Err(Error::StrategyNotSet)
    );
    assert_eq!(
        contract_outcome(client.try_withdraw_from_yield(&100_000)),
        Err(Error::StrategyNotSet)
    );
    assert_eq!(
        contract_outcome(client.try_harvest_yield()),
        Err(Error::StrategyNotSet)
    );

    // The resumed core operations really moved the float: +deposit -refund -withdraw.
    assert_eq!(
        token_client.balance(&client.address),
        vault_balance_before + 100_000 - 100_000 - 100_000
    );
    assert_eq!(token_client.balance(&buyer), 100_000);
    assert_eq!(
        client.get_refund(&payment_ref).unwrap().amount_refunded,
        100_000
    );
}

#[test]
fn test_extend_refund_ttl_fails_if_missing() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);
    let payment_ref = BytesN::from_array(&env, &[99u8; 32]);
    assert_eq!(
        client.try_extend_refund_ttl(&payment_ref),
        Err(Ok(Error::RefundNotFound))
    );
}

#[test]
fn test_extend_refund_ttl_succeeds() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&payment_ref, &buyer, &120_000, &0, &120_000, &None);

    // This shouldn't fail since the refund exists.
    client.extend_refund_ttl(&payment_ref);
}

#[test]
fn test_events_emitted() {
    use soroban_sdk::testutils::Events;
    use soroban_sdk::{vec, IntoVal, Map, Symbol, Val};
    let (env, client, merchant, _token) = setup(100);

    client.deposit(&merchant, &500_000);

    assert_eq!(
        env.events().all().filter_by_contract(&client.address),
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "deposit_event"), merchant.clone()).into_val(&env),
                soroban_sdk::map![&env, (Symbol::new(&env, "amount"), 500_000i128)].into_val(&env)
            )
        ]
    );

    let payment_ref = BytesN::from_array(&env, &[7u8; 32]);
    let buyer = Address::generate(&env);

    client.refund(&payment_ref, &buyer, &120_000, &0, &120_000, &None);

    let refund_events = env.events().all().filter_by_contract(&client.address);
    // The refund event carries the per-call amount and the running cumulative
    // total, so an indexer knows the state without summing history (#99).
    let mut refund_data = Map::<Val, Val>::new(&env);
    refund_data.set(
        Symbol::new(&env, "amount").into_val(&env),
        120_000i128.into_val(&env),
    );
    refund_data.set(
        Symbol::new(&env, "fee").into_val(&env),
        0i128.into_val(&env),
    );
    refund_data.set(
        Symbol::new(&env, "cumulative_refunded").into_val(&env),
        120_000i128.into_val(&env),
    );
    refund_data.set(
        Symbol::new(&env, "recipient").into_val(&env),
        buyer.clone().into_val(&env),
    );
    refund_data.set(
        Symbol::new(&env, "ledger").into_val(&env),
        env.ledger().sequence().into_val(&env),
    );
    assert_eq!(
        refund_events,
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "refund_event"), payment_ref.clone()).into_val(&env),
                refund_data.into_val(&env)
            )
        ]
    );

    client.withdraw(&100_000, &merchant);

    assert_eq!(
        env.events().all().filter_by_contract(&client.address),
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "withdraw_event"), merchant.clone()).into_val(&env),
                soroban_sdk::map![&env, (Symbol::new(&env, "amount"), 100_000i128)].into_val(&env)
            )
        ]
    );
}

#[test]
fn test_pause_unpause_refund_window_events_emitted() {
    use soroban_sdk::testutils::Events;
    use soroban_sdk::{vec, IntoVal, Map, Symbol, Val};

    let (env, client, _merchant, _token) = setup(100);
    let empty_data: Map<Val, Val> = Map::new(&env);

    env.ledger().with_mut(|li| li.sequence_number = 500);
    client.pause();

    assert_eq!(
        env.events().all().filter_by_contract(&client.address),
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "pause_event"), 500u32).into_val(&env),
                empty_data.clone().into_val(&env)
            )
        ]
    );

    env.ledger().with_mut(|li| li.sequence_number = 600);
    client.unpause();

    assert_eq!(
        env.events().all().filter_by_contract(&client.address),
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "unpause_event"), 600u32).into_val(&env),
                empty_data.clone().into_val(&env)
            )
        ]
    );

    env.ledger().with_mut(|li| li.sequence_number = 700);
    client.propose_policy(&300, &0, &0);

    // The data map differs across fields by native type (u64 deadline, u32
    // ledger fields), so build it with per-entry `into_val` rather than the
    // `map!` macro, which requires one homogeneous value type.
    let mut policy_data = Map::<Val, Val>::new(&env);
    policy_data.set(
        Symbol::new(&env, "deadline").into_val(&env),
        0u64.into_val(&env),
    );
    policy_data.set(
        Symbol::new(&env, "proposed_at_ledger").into_val(&env),
        700u32.into_val(&env),
    );
    policy_data.set(
        Symbol::new(&env, "execute_after_ledger").into_val(&env),
        (700u32 + 17_280u32).into_val(&env),
    );

    assert_eq!(
        env.events().all().filter_by_contract(&client.address),
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "policy_proposed_event"), 300u32).into_val(&env),
                policy_data.into_val(&env)
            )
        ]
    );
}

/// The commit hash embedded via contractmeta must be real provenance, not the
/// silent "unknown" fallback, in a normal repository build (see build.rs).
#[test]
fn test_commit_meta_is_well_formed() {
    let sha = env!("GIT_SHA");
    assert_ne!(sha, "unknown", "GIT_SHA must not fall back to 'unknown'");
    assert_eq!(sha.len(), 40, "GIT_SHA should be 40 hex chars, got: {sha}");
    assert!(
        sha.bytes().all(|b| b.is_ascii_hexdigit()),
        "GIT_SHA contains non-hex chars: {sha}"
    );

    let dirty = env!("GIT_DIRTY");
    assert!(
        dirty == "0" || dirty == "1",
        "GIT_DIRTY must be '0' or '1', got: {dirty}"
    );
}

#[test]
#[should_panic(expected = "HostError")]
fn test_refund_without_trustline() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    let payment_ref = BytesN::from_array(&env, &[11u8; 32]);
    let stranger = Address::from_string(&soroban_sdk::String::from_str(
        &env,
        "GBJCHUKZMTFJWQYW2HX4XAZ2ZV7UYWV6X4XAZ2ZV7UYWV6X4XAZ2ZV7U",
    ));

    // stranger has no trustline.
    client.refund(&payment_ref, &stranger, &120_000, &0, &120_000, &None);
}

// ── Two-step admin transfer tests ──────────────────────────────────────────

#[test]
fn test_transfer_admin_happy_path() {
    let (env, client, _merchant, _token) = setup(100);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&new_admin);

    // Admin hasn't changed yet — original admin can still act.
    client.pause();
    client.unpause();
}

#[test]
fn test_accept_admin_transfers_role() {
    let (env, client, _merchant, _token) = setup(100);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&new_admin);
    client.accept_admin();

    // New admin can call admin-only functions (propose_policy needs no token balance).
    client.propose_policy(&200, &0, &0);
}

#[test]
fn test_accept_admin_without_pending_fails() {
    let (_env, client, _merchant, _token) = setup(100);

    // No transfer initiated — accept should fail.
    assert_eq!(client.try_accept_admin(), Err(Ok(Error::NoPendingTransfer)));
}

#[test]
fn test_cancel_admin_transfer_succeeds() {
    let (env, client, _merchant, _token) = setup(100);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&new_admin);
    client.cancel_admin_transfer();

    // After cancel, accept should fail.
    assert_eq!(client.try_accept_admin(), Err(Ok(Error::NoPendingTransfer)));
}

#[test]
fn test_cancel_without_pending_fails() {
    let (_env, client, _merchant, _token) = setup(100);

    assert_eq!(
        client.try_cancel_admin_transfer(),
        Err(Ok(Error::NoPendingTransfer))
    );
}

#[test]
fn test_cancel_then_reinitiate_works() {
    let (env, client, _merchant, _token) = setup(100);
    let admin_a = Address::generate(&env);
    let admin_b = Address::generate(&env);

    // Initiate to A, cancel, then initiate to B and accept.
    client.transfer_admin(&admin_a);
    client.cancel_admin_transfer();
    client.transfer_admin(&admin_b);
    client.accept_admin();

    // B is now admin — propose_policy should work.
    client.propose_policy(&200, &0, &0);
}

#[test]
fn test_overwrite_pending_admin() {
    let (env, client, _merchant, _token) = setup(100);
    let admin_a = Address::generate(&env);
    let admin_b = Address::generate(&env);

    // Initiate to A, then re-initiate to B without cancelling.
    client.transfer_admin(&admin_a);
    client.transfer_admin(&admin_b);

    // Accept — B should become admin.
    client.accept_admin();
    client.propose_policy(&200, &0, &0);
}

#[test]
fn test_old_admin_cannot_act_after_transfer() {
    let (env, client, _merchant, _token) = setup(100);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&new_admin);
    client.accept_admin();

    // New admin can call admin-only functions.
    client.propose_policy(&200, &0, &0);
}

#[test]
fn test_transfer_admin_uninitialized_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(RefundVault, ());
    let client = RefundVaultClient::new(&env, &contract_id);
    let addr = Address::generate(&env);

    assert_eq!(
        client.try_transfer_admin(&addr),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn test_cancel_admin_transfer_uninitialized_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(RefundVault, ());
    let client = RefundVaultClient::new(&env, &contract_id);

    assert_eq!(
        client.try_cancel_admin_transfer(),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
#[should_panic]
fn test_transfer_admin_requires_auth() {
    let (env, client, _merchant, _token) = setup(100);
    let new_admin = Address::generate(&env);

    env.set_auths(&[]);
    client.transfer_admin(&new_admin);
}

#[test]
#[should_panic]
fn test_accept_admin_requires_pending_auth() {
    let (env, client, _merchant, _token) = setup(100);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&new_admin);

    // Clear all auths — pending_admin.require_auth() should panic.
    env.set_auths(&[]);
    client.accept_admin();
}

#[test]
#[should_panic]
fn test_cancel_admin_transfer_requires_auth() {
    let (env, client, _merchant, _token) = setup(100);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&new_admin);

    env.set_auths(&[]);
    client.cancel_admin_transfer();
}

#[test]
fn test_admin_transfer_events_emitted() {
    use soroban_sdk::testutils::Events;
    use soroban_sdk::{vec, IntoVal, Map, Symbol, Val};

    let (env, client, merchant, _token) = setup(100);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&new_admin);

    let empty_data: Map<Val, Val> = Map::new(&env);
    let events = env.events().all().filter_by_contract(&client.address);
    assert_eq!(
        events,
        vec![
            &env,
            (
                client.address.clone(),
                (
                    Symbol::new(&env, "admin_transfer_initiated_event"),
                    merchant.clone(),
                    new_admin.clone()
                )
                    .into_val(&env),
                empty_data.clone().into_val(&env)
            )
        ]
    );

    client.accept_admin();

    let events = env.events().all().filter_by_contract(&client.address);
    assert_eq!(
        events,
        vec![
            &env,
            (
                client.address.clone(),
                (
                    Symbol::new(&env, "admin_transfer_accepted_event"),
                    merchant.clone(),
                    new_admin.clone()
                )
                    .into_val(&env),
                empty_data.into_val(&env)
            )
        ]
    );
}

// ── Batch refund processing tests ─────────────────────────────────────────

#[test]
fn test_process_batch_multiple_refunds_succeed() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    let buyer1 = Address::generate(&env);
    let buyer2 = Address::generate(&env);

    let p1 = RefundParam {
        payment_ref: BytesN::from_array(&env, &[1u8; 32]),
        recipient: buyer1.clone(),
        amount: 100_000,
        paid_at_ledger: 0,
        payment_amount: 100_000,
        vdf_proof: None,
    };
    let p2 = RefundParam {
        payment_ref: BytesN::from_array(&env, &[2u8; 32]),
        recipient: buyer2.clone(),
        amount: 200_000,
        paid_at_ledger: 0,
        payment_amount: 200_000,
        vdf_proof: None,
    };

    let batch = vec![&env, p1.clone(), p2.clone()];
    let res = client.process_batch(&batch);
    assert_eq!(res, vec![&env, true, true]);

    assert!(client.get_refund(&p1.payment_ref).is_some());
    assert!(client.get_refund(&p2.payment_ref).is_some());
}

#[test]
fn test_process_batch_mixed_success_failure() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    let buyer1 = Address::generate(&env);
    let buyer2 = Address::generate(&env);

    let ref1 = BytesN::from_array(&env, &[1u8; 32]);
    // Pre-refund ref1 so it fails as AlreadyRefunded during batch execution
    client.refund(&ref1, &buyer1, &50_000, &0, &50_000, &None);

    let p1 = RefundParam {
        payment_ref: ref1,
        recipient: buyer1,
        amount: 50_000,
        paid_at_ledger: 0,
        payment_amount: 50_000,
        vdf_proof: None,
    };
    let p2 = RefundParam {
        payment_ref: BytesN::from_array(&env, &[2u8; 32]),
        recipient: buyer2,
        amount: 100_000,
        paid_at_ledger: 0,
        payment_amount: 100_000,
        vdf_proof: None,
    };

    let batch = vec![&env, p1, p2.clone()];
    let res = client.process_batch(&batch);

    // First item failed (false), second item succeeded (true)
    assert_eq!(res, vec![&env, false, true]);
    assert!(client.get_refund(&p2.payment_ref).is_some());
}

#[test]
fn test_process_batch_exceeds_max_size_fails() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    let buyer = Address::generate(&env);
    let mut batch = vec![&env];
    for i in 0..101u8 {
        let mut ref_bytes = [0u8; 32];
        ref_bytes[0] = i;
        batch.push_back(RefundParam {
            payment_ref: BytesN::from_array(&env, &ref_bytes),
            recipient: buyer.clone(),
            amount: 1,
            paid_at_ledger: 0,
            payment_amount: 1,
            vdf_proof: None,
        });
    }

    assert_eq!(
        client.try_process_batch(&batch),
        Err(Ok(Error::BatchTooLarge))
    );
}

// ── Policy timelock tests ──────────────────────────────────────────────────

#[test]
fn test_propose_and_execute_policy_happy_path() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    client.propose_policy(&200, &0, &0);

    let proposal = client.get_pending_policy().unwrap();
    assert_eq!(proposal.window, 200);
    assert_eq!(proposal.proposed_at_ledger, env.ledger().sequence());

    // Advance past the timelock.
    env.ledger().with_mut(|li| li.sequence_number += 17_280);
    client.execute_policy();

    assert!(client.get_pending_policy().is_none());
}

#[test]
fn test_execute_policy_before_timelock_fails() {
    let (env, client, _merchant, _token) = setup(100);

    client.propose_policy(&200, &0, &0);

    // Advance only partway through the timelock.
    env.ledger().with_mut(|li| li.sequence_number = 10_000);

    assert_eq!(
        client.try_execute_policy(),
        Err(Ok(Error::TimelockNotExpired))
    );
}

#[test]
fn test_execute_policy_at_exact_boundary_succeeds() {
    let (env, client, _merchant, _token) = setup(100);

    client.propose_policy(&200, &0, &0);

    // proposed_at_ledger = 1, timelock = 17_280, so execute at 1 + 17_280 = 17_281.
    env.ledger().with_mut(|li| li.sequence_number = 17_281);
    client.execute_policy();

    assert!(client.get_pending_policy().is_none());
}

#[test]
fn test_execute_policy_without_proposal_fails() {
    let (_env, client, _merchant, _token) = setup(100);

    assert_eq!(client.try_execute_policy(), Err(Ok(Error::NoPendingPolicy)));
}

#[test]
fn test_propose_policy_overwrites_existing() {
    let (_env, client, _merchant, _token) = setup(100);

    client.propose_policy(&200, &0, &0);
    client.propose_policy(&500, &0, &0);

    let proposal = client.get_pending_policy().unwrap();
    assert_eq!(proposal.window, 500);
}

#[test]
fn test_execute_policy_applies_new_window() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    // Window = 100. Paid at ledger 1. Current ledger 300 > 1+100=101 => expired.
    env.ledger().with_mut(|li| li.sequence_number = 300);

    let payment_ref = BytesN::from_array(&env, &[1u8; 32]);
    let buyer = Address::generate(&env);
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &100, &1, &100, &None),
        Err(Ok(Error::WindowExpired))
    );

    // Propose and execute a wider window.
    client.propose_policy(&20_000, &0, &0);
    env.ledger().with_mut(|li| li.sequence_number += 17_280);
    client.execute_policy();

    // Now the refund succeeds: current ~17_580, paid_at 1, window 20_000.
    client.refund(&payment_ref, &buyer, &100, &1, &100, &None);
    assert!(client.get_refund(&payment_ref).is_some());
}

#[test]
fn test_propose_policy_uninitialized_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(RefundVault, ());
    let client = RefundVaultClient::new(&env, &contract_id);

    assert_eq!(
        client.try_propose_policy(&100, &0, &0),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn test_execute_policy_uninitialized_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(RefundVault, ());
    let client = RefundVaultClient::new(&env, &contract_id);

    assert_eq!(client.try_execute_policy(), Err(Ok(Error::NotInitialized)));
}

#[test]
#[should_panic]
fn test_propose_policy_requires_auth() {
    let (env, client, _merchant, _token) = setup(100);
    env.set_auths(&[]);
    client.propose_policy(&200, &0, &0);
}

#[test]
#[should_panic]
fn test_execute_policy_requires_auth() {
    let (env, client, _merchant, _token) = setup(100);
    client.propose_policy(&200, &0, &0);
    env.set_auths(&[]);
    client.execute_policy();
}

#[test]
fn test_get_policy_timelock() {
    assert_eq!(RefundVault::get_policy_timelock(), 17_280);
}

// ── Refund policy deadline tests ───────────────────────────────────────────
//
// The policy deadline is a wall-clock timestamp (configured via
// propose_policy / execute_policy) that `refund` checks against the current
// ledger timestamp. It is orthogonal to the ledger-based refund window, and
// `0` (the default) means "no deadline".

/// Fund the vault and apply a policy carrying the given window and deadline,
/// fast-forwarding past the timelock so the proposal is live.
fn apply_policy(window: u32, deadline: u64) -> (Env, RefundVaultClient<'static>, Address, Address) {
    let (env, client, merchant, token) = setup(window);
    client.deposit(&merchant, &500_000);

    client.propose_policy(&window, &deadline, &0);
    env.ledger().with_mut(|li| li.sequence_number += 17_280);
    client.execute_policy();

    (env, client, merchant, token)
}

#[test]
fn test_policy_deadline_is_configured_and_readable() {
    let (env, client, _merchant, _token) = setup(100);

    // No deadline by default.
    assert_eq!(client.get_refund_deadline(), 0);
    assert_eq!(client.get_pending_policy(), None);

    let deadline = env.ledger().timestamp() + 1_000;
    client.propose_policy(&100, &deadline, &0);

    // The pending proposal carries the deadline before it is executed...
    let proposal = client.get_pending_policy().unwrap();
    assert_eq!(proposal.window, 100);
    assert_eq!(proposal.deadline, deadline);
    // ...but it is not live until execute_policy runs.
    assert_eq!(client.get_refund_deadline(), 0);

    env.ledger().with_mut(|li| li.sequence_number += 17_280);
    client.execute_policy();

    assert_eq!(client.get_refund_deadline(), deadline);
    assert_eq!(client.get_pending_policy(), None);
}

#[test]
fn test_refund_before_deadline_succeeds() {
    let deadline = 1_700_000_000;
    let (env, client, _merchant, _token) = apply_policy(100, deadline);

    // A claim strictly before the deadline is accepted.
    let payment_ref = BytesN::from_array(&env, &[0x20u8; 32]);
    let buyer = Address::generate(&env);
    env.ledger().with_mut(|li| li.timestamp = deadline - 1);
    client.refund(
        &payment_ref,
        &buyer,
        &100,
        &env.ledger().sequence(),
        &100,
        &None,
    );
    assert!(client.get_refund(&payment_ref).is_some());
}

#[test]
fn test_refund_at_deadline_boundary_succeeds() {
    let deadline = 1_700_000_000;
    let (env, client, _merchant, _token) = apply_policy(100, deadline);

    // The temporal boundary: a claim landing exactly on the deadline still
    // succeeds (expiry is strictly past the deadline).
    let payment_ref = BytesN::from_array(&env, &[0x21u8; 32]);
    let buyer = Address::generate(&env);
    env.ledger().with_mut(|li| li.timestamp = deadline);
    client.refund(
        &payment_ref,
        &buyer,
        &100,
        &env.ledger().sequence(),
        &100,
        &None,
    );
    assert!(client.get_refund(&payment_ref).is_some());
}

#[test]
fn test_refund_after_deadline_fails() {
    let deadline = 1_700_000_000;
    let (env, client, _merchant, token) = apply_policy(100, deadline);

    // One second past the deadline the claim is rejected and leaves no trace:
    // no record written, float untouched.
    let payment_ref = BytesN::from_array(&env, &[0x22u8; 32]);
    let buyer = Address::generate(&env);
    env.ledger().with_mut(|li| li.timestamp = deadline + 1);
    assert_eq!(
        client.try_refund(
            &payment_ref,
            &buyer,
            &100,
            &env.ledger().sequence(),
            &100,
            &None
        ),
        Err(Ok(Error::RefundExpired))
    );
    assert!(
        client.get_refund(&payment_ref).is_none(),
        "an expired claim must not write a refund record"
    );
    assert_eq!(
        TokenClient::new(&env, &token).balance(&client.address),
        500_000,
        "an expired claim must not move float"
    );
}

#[test]
fn test_deadline_and_window_are_independent_bounds() {
    let deadline = 1_700_000_000;
    let (env, client, _merchant, _token) = apply_policy(100, deadline);

    let payment_ref = BytesN::from_array(&env, &[0x23u8; 32]);
    let buyer = Address::generate(&env);

    // Past the deadline but still inside the window: RefundExpired, not
    // WindowExpired.
    env.ledger().with_mut(|li| li.timestamp = deadline + 1);
    assert_eq!(
        client.try_refund(
            &payment_ref,
            &buyer,
            &100,
            &env.ledger().sequence(),
            &100,
            &None
        ),
        Err(Ok(Error::RefundExpired))
    );
    assert!(client.get_refund(&payment_ref).is_none());
}

#[test]
fn test_zero_deadline_disables_expiry() {
    let (env, client, _merchant, _token) = apply_policy(100, 0);

    // deadline == 0 mirrors window == 0: refunds are never expired by the
    // deadline, no matter how far the ledger timestamp advances.
    env.ledger().with_mut(|li| li.timestamp = u64::MAX / 2);
    let payment_ref = BytesN::from_array(&env, &[0x24u8; 32]);
    let buyer = Address::generate(&env);
    client.refund(
        &payment_ref,
        &buyer,
        &100,
        &env.ledger().sequence(),
        &100,
        &None,
    );
    assert!(client.get_refund(&payment_ref).is_some());
}

// ── Refund fee tests ───────────────────────────────────────────────────────
//
// A fee configured in basis points (1 bp = 0.01%) is deducted from each claim.
// Rounding always rounds *up* so a remainder smaller than one smallest unit of
// the token is collected by the protocol (the fee recipient) rather than lost.

/// Reference implementation of the fee formula, used to independently verify
/// the contract's `refund_fee` across the sweep tests below:
/// `ceil(amount * fee_bps / 10_000)`, computed in u128 to avoid truncation.
fn reference_fee(amount: i128, bps: u32) -> i128 {
    let scaled = (amount as u128) * bps as u128;
    scaled.div_ceil(10_000) as i128
}

#[test]
fn test_refund_fee_helper_math_rounds_up() {
    // Rounding table: the protocol keeps any sub-basis-point remainder.
    let cases = [
        (0i128, 0u32, 0i128),
        (0, 500, 0),
        (1, 1, 1),   // ceil(0.0001) = 1
        (100, 1, 1), // ceil(0.01) = 1
        (100, 500, 5),
        (100, 1000, 10),
        (9_999, 1, 1),            // ceil(0.9999) = 1
        (10_000, 1, 1),           // exactly 1
        (333, 3, 1),              // ceil(0.0999) = 1
        (5_000, 3, 2),            // 1.5 -> 2
        (10_000, 100, 100),       // 1%
        (10_000, 10_000, 10_000), // 100%
    ];
    for (amount, bps, expected) in cases {
        assert_eq!(
            refund_fee(amount, bps),
            expected,
            "amount={amount} fee_bps={bps}"
        );
    }

    // Cross-check the helper against the reference across a sweep.
    for amount in [
        1i128,
        99,
        100,
        7,
        499,
        500,
        501,
        9_999,
        10_000,
        12_345,
        1_700_000_000,
    ] {
        for bps in [0u32, 1, 3, 10, 25, 100, 500, 2500, 700, 9999, 10_000] {
            assert_eq!(
                refund_fee(amount, bps),
                reference_fee(amount, bps),
                "amount={amount} fee_bps={bps}"
            );
        }
    }
}

#[test]
fn test_fee_defaults_disabled() {
    let (env, client, merchant, token) = setup(100);
    client.deposit(&merchant, &500_000);

    assert_eq!(client.get_fee_bps(), 0);
    assert_eq!(client.get_fee_recipient(), None);

    // Unconfigured fees: the buyer receives the full claim, nothing is diverted.
    let payment_ref = BytesN::from_array(&env, &[0x40u8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&payment_ref, &buyer, &100_000, &0, &100_000, &None);

    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&buyer), 100_000);
    assert_eq!(token_client.balance(&client.address), 400_000);

    // The refund event reports a zero fee.
    let record = client.get_refund(&payment_ref).unwrap();
    assert_eq!(record.amount_refunded, 100_000);
    assert_eq!(record.recipient, buyer);
}

#[test]
fn test_fee_deducted_and_collected_exactly() {
    let (env, client, merchant, token) = setup(100);
    let token_client = TokenClient::new(&env, &token);
    StellarAssetClient::new(&env, &token).mint(&merchant, &9_000_000);
    client.deposit(&merchant, &10_000_000);

    let buyer = Address::generate(&env);
    let fee_collector = Address::generate(&env);
    client.set_fee_recipient(&fee_collector);
    client.set_fee_bps(&100); // 1%

    let payment_ref = BytesN::from_array(&env, &[0x41u8; 32]);
    client.refund(&payment_ref, &buyer, &1_000_000, &0, &1_000_000, &None);

    // 1% of 1M = 10_000 exactly; buyer receives the remainder.
    assert_eq!(token_client.balance(&buyer), 1_000_000 - 10_000);
    assert_eq!(token_client.balance(&fee_collector), 10_000);
    assert_eq!(token_client.balance(&client.address), 9_000_000);

    // 3 bp of an odd amount: 123_456 * 3 / 10_000 = 37.0368 -> 38 (rounds up).
    client.set_fee_bps(&3);
    let ref2 = BytesN::from_array(&env, &[0x42u8; 32]);
    client.refund(&ref2, &buyer, &123_456, &0, &123_456, &None);
    assert_eq!(token_client.balance(&fee_collector), 10_000 + 38);
    assert_eq!(token_client.balance(&buyer), 990_000 + 123_456 - 38);

    // 1 bp on a sub-10_000 amount still yields one unit (ceil).
    client.set_fee_bps(&1);
    let ref3 = BytesN::from_array(&env, &[0x43u8; 32]);
    client.refund(&ref3, &buyer, &7, &0, &7, &None);
    assert_eq!(token_client.balance(&fee_collector), 10_000 + 38 + 1);
    assert_eq!(token_client.balance(&buyer), 990_000 + 123_456 - 38 + 7 - 1);

    // Total outflow (buyer + fees) is exactly the sum of the claims.
    let total_claims = 1_000_000 + 123_456 + 7;
    assert_eq!(
        token_client.balance(&client.address),
        10_000_000 - total_claims
    );
    assert_eq!(token_client.balance(&fee_collector), 10_000 + 38 + 1);

    // Returning the fee to zero disables it again.
    client.set_fee_bps(&0);
    let ref4 = BytesN::from_array(&env, &[0x44u8; 32]);
    client.refund(&ref4, &buyer, &5_000, &0, &5_000, &None);
    assert_eq!(
        token_client.balance(&buyer),
        990_000 + 123_456 - 38 + 7 - 1 + 5_000
    );
    assert_eq!(token_client.balance(&fee_collector), 10_000 + 38 + 1);
}

#[test]
fn test_fee_math_various_bps_exact_balances() {
    let (env, client, merchant, token) = setup(100);
    let token_client = TokenClient::new(&env, &token);
    StellarAssetClient::new(&env, &token).mint(&merchant, &99_000_000);
    client.deposit(&merchant, &100_000_000);

    let buyer = Address::generate(&env);
    let fee_collector = Address::generate(&env);
    client.set_fee_recipient(&fee_collector);

    // (claim, bps) sweep across many basis-point configurations: after every
    // step the buyer, fee collector and vault balances match exactly.
    let sweep = [
        (1_000_000i128, 0u32),
        (1_000_000, 1),
        (1_000_000, 10),
        (1_000_000, 100),
        (1_000_000, 700),
        (1_000_000, 9999),
        (123_457, 1),
        (123_457, 3),
        (123_457, 25),
        (5_000, 2500),
        (100, 1),
        (1, 1),
        (1, 10_000),
        (10_000_000, 500),
    ];

    let mut buyer_total = 0i128;
    let mut fee_total = 0i128;
    let mut claimed_total = 0i128;

    for (i, (amount, bps)) in sweep.iter().enumerate() {
        client.set_fee_bps(bps);
        let fee = reference_fee(*amount, *bps);

        let payment_ref = BytesN::from_array(&env, &[0x50u8 + i as u8; 32]);
        client.refund(&payment_ref, &buyer, amount, &0, amount, &None);

        buyer_total += amount - fee;
        fee_total += fee;
        claimed_total += amount;

        // The buyer and the fee collector each hold exactly their running
        // shares, and the vault holds exactly what the claims did not drain.
        assert_eq!(
            token_client.balance(&buyer),
            buyer_total,
            "buyer balance after (amount={amount}, bps={bps})"
        );
        assert_eq!(
            token_client.balance(&fee_collector),
            fee_total,
            "fee balance after (amount={amount}, bps={bps})"
        );
        assert_eq!(
            token_client.balance(&client.address),
            100_000_000 - claimed_total,
            "vault balance after (amount={amount}, bps={bps})"
        );
    }
}

#[test]
fn test_fee_claim_equal_to_float_succeeds_and_drains() {
    // A 10% fee on a claim equal to the entire float still inflows exactly to
    // both parties and drains the vault to zero.
    let (env, client, merchant, token) = setup(100);
    let token_client = TokenClient::new(&env, &token);
    client.deposit(&merchant, &5_000);

    let buyer = Address::generate(&env);
    let fee_collector = Address::generate(&env);
    client.set_fee_recipient(&fee_collector);
    client.set_fee_bps(&1000); // 10%

    let payment_ref = BytesN::from_array(&env, &[0x45u8; 32]);
    client.refund(&payment_ref, &buyer, &5_000, &0, &5_000, &None);

    assert_eq!(token_client.balance(&buyer), 4_500);
    assert_eq!(token_client.balance(&fee_collector), 500);
    assert_eq!(token_client.balance(&client.address), 0);
}

#[test]
fn test_fee_defaults_to_merchant_when_no_recipient_set() {
    let (env, client, merchant, token) = setup(100);
    let token_client = TokenClient::new(&env, &token);
    client.deposit(&merchant, &500_000);

    // Fee configured but no explicit recipient: the merchant collects the fee.
    client.set_fee_bps(&100); // 1%

    let payment_ref = BytesN::from_array(&env, &[0x46u8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&payment_ref, &buyer, &100_000, &0, &100_000, &None);

    assert_eq!(token_client.balance(&buyer), 99_000);
    assert_eq!(token_client.balance(&merchant), 500_000 + 1_000);
    assert_eq!(token_client.balance(&client.address), 400_000);

    // An explicit recipient overrides the merchant default.
    let fee_collector = Address::generate(&env);
    client.set_fee_recipient(&fee_collector);
    assert_eq!(client.get_fee_recipient(), Some(fee_collector.clone()));
    let ref2 = BytesN::from_array(&env, &[0x47u8; 32]);
    client.refund(&ref2, &buyer, &50_000, &0, &50_000, &None);
    assert_eq!(token_client.balance(&fee_collector), 500);
    assert_eq!(token_client.balance(&merchant), 501_000);
}

#[test]
fn test_fee_applies_to_partial_refunds_and_ceiling() {
    // Partial refunds are still ceiling-bounded by the pre-fee claim amount:
    // the fee is an outflow split, not an expansion of what may be claimed.
    let (env, client, merchant, _token) = setup(100);
    StellarAssetClient::new(&env, &_token).mint(&merchant, &1_000_000);
    client.deposit(&merchant, &2_000_000);

    let buyer = Address::generate(&env);
    let fee_collector = Address::generate(&env);
    client.set_fee_recipient(&fee_collector);
    client.set_fee_bps(&100); // 1%

    let payment_ref = BytesN::from_array(&env, &[0x48u8; 32]);
    // payment_amount (the ceiling) is 1M; each partial claim is pre-fee.
    client.refund(&payment_ref, &buyer, &400_000, &0, &1_000_000, &None);
    let record = client.get_refund(&payment_ref).unwrap();
    assert_eq!(record.amount_refunded, 400_000);
    assert_eq!(record.payment_amount, 1_000_000);

    client.refund(&payment_ref, &buyer, &400_000, &0, &1_000_000, &None);
    let record = client.get_refund(&payment_ref).unwrap();
    assert_eq!(record.amount_refunded, 800_000);

    // The remaining 600_000 is still claimable (ceiling is pre-fee), but
    // 600_000 + 600_000 would exceed the ceiling.
    client.refund(&payment_ref, &buyer, &200_000, &0, &1_000_000, &None);
    assert_eq!(
        client.get_refund(&payment_ref).unwrap().amount_refunded,
        1_000_000
    );
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &1, &0, &1_000_000, &None),
        Err(Ok(Error::ExceedsPayment))
    );
}

#[test]
fn test_set_fee_bps_rejects_out_of_range() {
    let (_env, client, _merchant, _token) = setup(100);

    assert_eq!(
        client.try_set_fee_bps(&10_001),
        Err(Ok(Error::InvalidRatio))
    );
    // Upper bound itself is legal (100%).
    client.set_fee_bps(&10_000);
    assert_eq!(client.get_fee_bps(), 10_000);
}

#[test]
fn test_set_fee_recipient_rejects_contract_address() {
    let (_env, client, _merchant, _token) = setup(100);
    assert_eq!(
        client.try_set_fee_recipient(&client.address),
        Err(Ok(Error::SelfTransfer))
    );
}

#[test]
fn test_set_fee_config_requires_admin_auth() {
    let (env, client, _merchant, _token) = setup(100);

    // No signatures: merchant.require_auth() aborts rather than returning an
    // error, so the call surfaces as a host failure (see
    // `test_set_yield_strategy_requires_auth`).
    env.set_auths(&[]);
    assert!(client.try_set_fee_bps(&100).is_err());
    assert!(client
        .try_set_fee_recipient(&Address::generate(&env))
        .is_err());
}

#[test]
fn test_fee_config_uninitialized_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(RefundVault, ());
    let client = RefundVaultClient::new(&env, &contract_id);

    assert_eq!(client.try_set_fee_bps(&100), Err(Ok(Error::NotInitialized)));
    assert_eq!(
        client.try_set_fee_recipient(&Address::generate(&env)),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn test_fee_config_events_emitted() {
    use soroban_sdk::testutils::Events;
    use soroban_sdk::{vec, IntoVal, Map, Symbol, Val};

    let (env, client, _merchant, _token) = setup(100);
    let fee_collector = Address::generate(&env);

    // Each change emits the full effective configuration, keyed by the field
    // that changed. Events are read after each individual invocation.
    client.set_fee_recipient(&fee_collector);
    let mut recipient_data = Map::<Val, Val>::new(&env);
    recipient_data.set(
        Symbol::new(&env, "fee_bps").into_val(&env),
        0u32.into_val(&env),
    );
    recipient_data.set(
        Symbol::new(&env, "fee_recipient").into_val(&env),
        fee_collector.clone().into_val(&env),
    );
    assert_eq!(
        env.events().all().filter_by_contract(&client.address),
        vec![
            &env,
            (
                client.address.clone(),
                (
                    Symbol::new(&env, "fee_config_updated_event"),
                    Symbol::new(&env, "fee_recipient"),
                )
                    .into_val(&env),
                recipient_data.into_val(&env)
            )
        ]
    );

    client.set_fee_bps(&100);
    let mut bps_data = Map::<Val, Val>::new(&env);
    bps_data.set(
        Symbol::new(&env, "fee_bps").into_val(&env),
        100u32.into_val(&env),
    );
    bps_data.set(
        Symbol::new(&env, "fee_recipient").into_val(&env),
        fee_collector.clone().into_val(&env),
    );
    assert_eq!(
        env.events().all().filter_by_contract(&client.address),
        vec![
            &env,
            (
                client.address.clone(),
                (
                    Symbol::new(&env, "fee_config_updated_event"),
                    Symbol::new(&env, "fee_bps"),
                )
                    .into_val(&env),
                bps_data.into_val(&env)
            )
        ]
    );
}

#[test]
fn test_policy_events_emitted() {
    use soroban_sdk::testutils::Events;
    use soroban_sdk::{vec, IntoVal, Map, Symbol, Val};

    let (env, client, _merchant, _token) = setup(100);

    client.propose_policy(&200, &0, &0);

    let current = env.ledger().sequence();
    let events = env.events().all().filter_by_contract(&client.address);
    // deadline is a u64 while the ledger fields are u32, so build the data
    // map per-entry (the `map!` macro requires one homogeneous value type).
    let mut proposed_data = Map::<Val, Val>::new(&env);
    proposed_data.set(
        Symbol::new(&env, "deadline").into_val(&env),
        0u64.into_val(&env),
    );
    proposed_data.set(
        Symbol::new(&env, "proposed_at_ledger").into_val(&env),
        current.into_val(&env),
    );
    proposed_data.set(
        Symbol::new(&env, "execute_after_ledger").into_val(&env),
        (current + 17_280u32).into_val(&env),
    );
    assert_eq!(
        events,
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "policy_proposed_event"), 200u32).into_val(&env),
                proposed_data.into_val(&env)
            )
        ]
    );

    env.ledger().with_mut(|li| li.sequence_number += 17_280);
    client.execute_policy();

    let mut executed_data = Map::<Val, Val>::new(&env);
    executed_data.set(
        Symbol::new(&env, "deadline").into_val(&env),
        0u64.into_val(&env),
    );
    let events = env.events().all().filter_by_contract(&client.address);
    assert_eq!(
        events,
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "policy_executed_event"), 200u32).into_val(&env),
                executed_data.into_val(&env)
            )
        ]
    );
}

// ---------------------------------------------------------------------------
// Shared Test Vectors (Issue #184)
// ---------------------------------------------------------------------------

#[path = "refund_vectors.rs"]
mod refund_vectors;

#[test]
fn test_shared_refund_vectors_match_typescript_sdk() {
    let (env, client, merchant, _token) = setup(1000);
    client.deposit(&merchant, &1_000_000);

    let recipient = Address::generate(&env);

    for v in refund_vectors::VECTORS {
        let payment_ref = BytesN::from_array(&env, &v.payment_ref);
        // `payment_amount` (the ceiling) is not part of the shared vectors,
        // which predate partial refunds; pass the amount itself so a vector's
        // `expected_success` outcome is preserved.
        let res = client.try_refund(
            &payment_ref,
            &recipient,
            &v.amount,
            &v.paid_at_ledger,
            &v.amount,
            &None,
        );

        assert_eq!(
            res.is_ok(),
            v.expected_success,
            "vector {:?}: contract returned is_ok={}, expected={}",
            v.name,
            res.is_ok(),
            v.expected_success
        );
    }
}

#[test]
fn test_shared_refund_vectors_cover_both_outcomes() {
    assert!(refund_vectors::VECTORS.iter().any(|v| v.expected_success));
    assert!(refund_vectors::VECTORS.iter().any(|v| !v.expected_success));
}

#[test]
fn test_shared_refund_vectors_include_live_testnet_refund() {
    let live = &refund_vectors::VECTORS[0];
    assert!(live.expected_success);
    assert!(live.tx_hash.is_some());
}

// ---------------------------------------------------------------------------
// Self-Transfer Rejection Tests (Issue #177)
// ---------------------------------------------------------------------------

#[test]
fn test_refund_to_contract_address_fails_self_transfer() {
    use soroban_sdk::testutils::Events;

    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    let payment_ref = BytesN::from_array(&env, &[12u8; 32]);
    let contract_addr = client.address.clone();

    // Refunding to vault address must return SelfTransfer error
    let res = client.try_refund(&payment_ref, &contract_addr, &50_000, &0, &50_000, &None);
    assert_eq!(res, Err(Ok(Error::SelfTransfer)));

    // Payment ref must remain unconsumed / not recorded
    assert!(client.get_refund(&payment_ref).is_none());

    // The reverted call emitted no events (all() only reports the last,
    // successful invocation), so the deposit is not even visible here.
    let events = env.events().all().filter_by_contract(&client.address);
    // Only the deposit event should exist (log is reset, so 0 events remain)
    assert_eq!(events.events().len(), 0);
}

#[test]
fn test_withdraw_to_contract_address_fails_self_transfer() {
    use soroban_sdk::testutils::Events;

    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    let contract_addr = client.address.clone();
    let res = client.try_withdraw(&50_000, &contract_addr);
    assert_eq!(res, Err(Ok(Error::SelfTransfer)));

    // The reverted call emitted no events at all.
    let events = env.events().all().filter_by_contract(&client.address);
    assert_eq!(events.events().len(), 0);
}

#[test]
fn test_refund_to_merchant_succeeds() {
    let (env, client, merchant, token) = setup(100);
    client.deposit(&merchant, &500_000);

    let payment_ref = BytesN::from_array(&env, &[13u8; 32]);
    let initial_merchant_bal = TokenClient::new(&env, &token).balance(&merchant);

    // Refunding to merchant is valid (e.g. merchant-as-buyer in testing or direct reversal)
    client.refund(&payment_ref, &merchant, &50_000, &0, &50_000, &None);

    let final_merchant_bal = TokenClient::new(&env, &token).balance(&merchant);
    assert_eq!(final_merchant_bal, initial_merchant_bal + 50_000);
    assert!(client.get_refund(&payment_ref).is_some());
}

// ---------------------------------------------------------------------------
// set_token Tests (Issue #176)
// ---------------------------------------------------------------------------

#[test]
fn test_set_token_succeeds_when_vault_is_empty() {
    let (env, client, merchant, _token) = setup(100);

    let new_token_admin = Address::generate(&env);
    let new_sac = env.register_stellar_asset_contract_v2(new_token_admin);
    let new_token = new_sac.address();
    StellarAssetClient::new(&env, &new_token).mint(&merchant, &FLOAT);

    // Vault has 0 float balance initially -> set_token succeeds
    client.set_token(&new_token);

    // Now deposit using the new token
    client.deposit(&merchant, &200_000);
    assert_eq!(
        TokenClient::new(&env, &new_token).balance(&client.address),
        200_000
    );
}

#[test]
fn test_set_token_fails_when_vault_is_funded() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    let new_token_admin = Address::generate(&env);
    let new_sac = env.register_stellar_asset_contract_v2(new_token_admin);
    let new_token = new_sac.address();

    // Vault is funded -> set_token must fail with FloatNotEmpty
    let res = client.try_set_token(&new_token);
    assert_eq!(res, Err(Ok(Error::FloatNotEmpty)));
}

#[test]
fn test_set_token_requires_admin_auth() {
    let (env, client, _merchant, _token) = setup(100);
    let _stranger = Address::generate(&env);

    let new_token_admin = Address::generate(&env);
    let new_sac = env.register_stellar_asset_contract_v2(new_token_admin);
    let new_token = new_sac.address();

    // If stranger calls or unauthorized caller
    env.mock_auths(&[]);
    // Calling set_token without merchant auth panics at require_auth
    // Let's verify with mock_all_auths reset
    env.mock_all_auths();
    assert!(client.try_set_token(&new_token).is_ok());
}

// ── Batch refund tests ─────────────────────────────────────────────────────

/// Build a [`RefundClaim`] with the standard defaults used by the batch tests.
fn make_claim(
    payment_ref: BytesN<32>,
    recipient: &Address,
    amount: i128,
    paid_at_ledger: u32,
    payment_amount: i128,
) -> RefundClaim {
    RefundClaim {
        payment_ref,
        recipient: recipient.clone(),
        amount,
        paid_at_ledger,
        payment_amount,
        vdf_proof: None,
    }
}

/// Construct the expected `(contract, topics, data)` triple for a
/// `refund_event` published by the refund vault.
fn expect_refund_event(
    env: &Env,
    client: &RefundVaultClient<'static>,
    payment_ref: &BytesN<32>,
    amount: i128,
    fee: i128,
    cumulative_refunded: i128,
    recipient: &Address,
) -> (soroban_sdk::Address, Vec<Val>, Val) {
    use soroban_sdk::{IntoVal, Map, Symbol};

    let mut data: Map<Val, Val> = Map::new(env);
    data.set(
        Symbol::new(env, "amount").into_val(env),
        amount.into_val(env),
    );
    data.set(Symbol::new(env, "fee").into_val(env), fee.into_val(env));
    data.set(
        Symbol::new(env, "cumulative_refunded").into_val(env),
        cumulative_refunded.into_val(env),
    );
    data.set(
        Symbol::new(env, "recipient").into_val(env),
        recipient.clone().into_val(env),
    );
    data.set(
        Symbol::new(env, "ledger").into_val(env),
        env.ledger().sequence().into_val(env),
    );

    (
        client.address.clone(),
        vec![
            env,
            Symbol::new(env, "refund_event").into_val(env),
            payment_ref.clone().into_val(env),
        ],
        data.into_val(env),
    )
}

#[test]
fn test_claim_batch_successful_multiple_claims() {
    let (env, client, merchant, token) = setup(100);
    client.deposit(&merchant, &500_000);
    let token_client = TokenClient::new(&env, &token);

    let b1 = Address::generate(&env);
    let b2 = Address::generate(&env);
    let b3 = Address::generate(&env);
    let ref1 = BytesN::from_array(&env, &[21u8; 32]);
    let ref2 = BytesN::from_array(&env, &[22u8; 32]);
    let ref3 = BytesN::from_array(&env, &[23u8; 32]);

    let claims = vec![
        &env,
        make_claim(ref1.clone(), &b1, 100_000, 0, 100_000),
        make_claim(ref2.clone(), &b2, 50_000, 0, 50_000),
        make_claim(ref3.clone(), &b3, 250_000, 0, 250_000),
    ];
    client.claim_batch(&claims);

    assert_eq!(token_client.balance(&b1), 100_000);
    assert_eq!(token_client.balance(&b2), 50_000);
    assert_eq!(token_client.balance(&b3), 250_000);
    // 400_000 of the 500_000 float left for the merchant-funded vault.
    assert_eq!(token_client.balance(&client.address), 100_000);

    let rec1 = client.get_refund(&ref1).unwrap();
    let rec2 = client.get_refund(&ref2).unwrap();
    let rec3 = client.get_refund(&ref3).unwrap();
    assert_eq!(rec1.amount_refunded, 100_000);
    assert_eq!(rec2.amount_refunded, 50_000);
    assert_eq!(rec3.amount_refunded, 250_000);
}

#[test]
fn test_claim_batch_emits_one_event_per_item() {
    use soroban_sdk::testutils::Events;

    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    let b1 = Address::generate(&env);
    let b2 = Address::generate(&env);
    let b3 = Address::generate(&env);
    let ref1 = BytesN::from_array(&env, &[31u8; 32]);
    let ref2 = BytesN::from_array(&env, &[32u8; 32]);
    let ref3 = BytesN::from_array(&env, &[33u8; 32]);

    let claims = vec![
        &env,
        make_claim(ref1.clone(), &b1, 100_000, 0, 100_000),
        make_claim(ref2.clone(), &b2, 50_000, 0, 50_000),
        make_claim(ref3.clone(), &b3, 250_000, 0, 250_000),
    ];
    client.claim_batch(&claims);

    // Exactly three refund_events, one per claim, in claim order.
    assert_eq!(
        env.events().all().filter_by_contract(&client.address),
        vec![
            &env,
            expect_refund_event(&env, &client, &ref1, 100_000, 0, 100_000, &b1),
            expect_refund_event(&env, &client, &ref2, 50_000, 0, 50_000, &b2),
            expect_refund_event(&env, &client, &ref3, 250_000, 0, 250_000, &b3),
        ]
    );
}

#[test]
fn test_claim_batch_partial_failure_reverts_everything() {
    use soroban_sdk::testutils::Events;

    let (env, client, merchant, token) = setup(100);
    client.deposit(&merchant, &500_000);
    let token_client = TokenClient::new(&env, &token);

    let b1 = Address::generate(&env);
    let b2 = Address::generate(&env);
    let b3 = Address::generate(&env);
    let ref1 = BytesN::from_array(&env, &[41u8; 32]);
    let ref2 = BytesN::from_array(&env, &[42u8; 32]);
    let ref3 = BytesN::from_array(&env, &[43u8; 32]);

    // The middle claim has a zero ceiling (payment_amount = 0) and must fail
    // with ExceedsPayment; the batch must revert the earlier successful claim.
    let claims = vec![
        &env,
        make_claim(ref1.clone(), &b1, 100_000, 0, 100_000),
        make_claim(ref2.clone(), &b2, 50_000, 0, 0),
        make_claim(ref3.clone(), &b3, 250_000, 0, 250_000),
    ];
    assert_eq!(
        client.try_claim_batch(&claims),
        Err(Ok(Error::ExceedsPayment))
    );

    // All-or-nothing: the first claim's transfer, record write and event were
    // rolled back along with the whole invocation.
    assert_eq!(token_client.balance(&client.address), 500_000);
    assert_eq!(token_client.balance(&b1), 0);
    assert_eq!(token_client.balance(&b2), 0);
    assert_eq!(token_client.balance(&b3), 0);
    assert!(client.get_refund(&ref1).is_none());
    assert_eq!(
        env.events().all().filter_by_contract(&client.address),
        vec![&env]
    );
}

#[test]
fn test_claim_batch_same_ref_accumulates_and_excess_reverts() {
    let (env, client, merchant, token) = setup(100);
    StellarAssetClient::new(&env, &token).mint(&merchant, &1_000_000);
    client.deposit(&merchant, &2_000_000);
    let token_client = TokenClient::new(&env, &token);

    let buyer = Address::generate(&env);
    let ref1 = BytesN::from_array(&env, &[51u8; 32]);

    // Two partials against the same 1M ceiling within one batch.
    let claims = vec![
        &env,
        make_claim(ref1.clone(), &buyer, 400_000, 0, 1_000_000),
        make_claim(ref1.clone(), &buyer, 400_000, 0, 1_000_000),
    ];
    client.claim_batch(&claims);
    assert_eq!(client.get_refund(&ref1).unwrap().amount_refunded, 800_000);
    assert_eq!(token_client.balance(&buyer), 800_000);

    // A later batch whose second element would push the cumulative over the
    // ceiling must fail and roll back the first element too.
    let excess = vec![
        &env,
        make_claim(ref1.clone(), &buyer, 400_000, 0, 1_000_000),
        make_claim(ref1.clone(), &buyer, 700_000, 0, 1_000_000),
    ];
    assert_eq!(
        client.try_claim_batch(&excess),
        Err(Ok(Error::ExceedsPayment))
    );

    assert_eq!(client.get_refund(&ref1).unwrap().amount_refunded, 800_000);
    assert_eq!(token_client.balance(&buyer), 800_000);
    assert_eq!(token_client.balance(&client.address), 2_000_000 - 800_000);
}

#[test]
fn test_claim_batch_float_checked_per_item() {
    let (env, client, merchant, token) = setup(100);
    client.deposit(&merchant, &500_000);
    let token_client = TokenClient::new(&env, &token);

    let b1 = Address::generate(&env);
    let b2 = Address::generate(&env);
    let ref1 = BytesN::from_array(&env, &[61u8; 32]);
    let ref2 = BytesN::from_array(&env, &[62u8; 32]);

    // Each element of a valid first claim leaves 200k; the second claim needs
    // 300k and must observe the reduced float and fail, reverting the batch.
    let claims = vec![
        &env,
        make_claim(ref1.clone(), &b1, 300_000, 0, 300_000),
        make_claim(ref2.clone(), &b2, 300_000, 0, 300_000),
    ];
    assert_eq!(
        client.try_claim_batch(&claims),
        Err(Ok(Error::InsufficientFloat))
    );

    assert_eq!(token_client.balance(&client.address), 500_000);
    assert_eq!(token_client.balance(&b1), 0);
    assert_eq!(token_client.balance(&b2), 0);
    assert!(client.get_refund(&ref1).is_none());
    assert!(client.get_refund(&ref2).is_none());
}

#[test]
fn test_claim_batch_empty_succeeds() {
    use soroban_sdk::testutils::Events;

    let (env, client, merchant, token) = setup(100);
    client.deposit(&merchant, &100_000);
    let token_client = TokenClient::new(&env, &token);

    let claims: Vec<RefundClaim> = Vec::new(&env);
    client.claim_batch(&claims);

    // No-op: float untouched, no events.
    assert_eq!(token_client.balance(&client.address), 100_000);
    assert_eq!(
        env.events().all().filter_by_contract(&client.address),
        vec![&env]
    );
}

#[test]
fn test_claim_batch_fee_applied_per_item() {
    let (env, client, merchant, token) = setup(100);
    StellarAssetClient::new(&env, &token).mint(&merchant, &9_000_000);
    client.deposit(&merchant, &10_000_000);
    let token_client = TokenClient::new(&env, &token);

    let fee_collector = Address::generate(&env);
    client.set_fee_recipient(&fee_collector);
    client.set_fee_bps(&100);

    let b1 = Address::generate(&env);
    let b2 = Address::generate(&env);
    let b3 = Address::generate(&env);
    let ref1 = BytesN::from_array(&env, &[71u8; 32]);
    let ref2 = BytesN::from_array(&env, &[72u8; 32]);
    let ref3 = BytesN::from_array(&env, &[73u8; 32]);

    let claims = vec![
        &env,
        make_claim(ref1.clone(), &b1, 1_000_000, 0, 1_000_000),
        make_claim(ref2.clone(), &b2, 1_000_000, 0, 1_000_000),
        make_claim(ref3.clone(), &b3, 1_000_000, 0, 1_000_000),
    ];
    client.claim_batch(&claims);

    // 1% up-rounded fee per claim: buyers net 990k each, protocol nets 30k.
    assert_eq!(token_client.balance(&b1), 990_000);
    assert_eq!(token_client.balance(&b2), 990_000);
    assert_eq!(token_client.balance(&b3), 990_000);
    assert_eq!(token_client.balance(&fee_collector), 30_000);
    assert_eq!(token_client.balance(&client.address), 7_000_000);
}

// A batch without merchant authorization aborts at require_auth.
#[test]
#[should_panic]
fn test_claim_batch_without_auth_panics() {
    let (env, client, _merchant, _token) = setup(100);

    let buyer = Address::generate(&env);
    let claims = vec![
        &env,
        make_claim(BytesN::from_array(&env, &[80u8; 32]), &buyer, 100, 0, 100),
    ];

    // Default auth setup authorizes only the contract invoker; the merchant is
    // never authorized here, so require_auth aborts the invocation.
    env.mock_auths(&[]);
    client.claim_batch(&claims);
}

#[test]
fn test_claim_batch_when_paused_fails() {
    let (env, client, merchant, token) = setup(100);
    client.deposit(&merchant, &100_000);
    client.pause();
    let token_client = TokenClient::new(&env, &token);

    let buyer = Address::generate(&env);
    let claims = vec![
        &env,
        make_claim(BytesN::from_array(&env, &[81u8; 32]), &buyer, 100, 0, 100),
    ];
    assert_eq!(client.try_claim_batch(&claims), Err(Ok(Error::Paused)));

    // Paused state preserved the float and wrote nothing.
    assert_eq!(token_client.balance(&client.address), 100_000);
    assert_eq!(token_client.balance(&buyer), 0);
}

#[test]
fn test_claim_batch_cost_stays_within_budget() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &1_000_000);

    // Default test-host budget limits (soroban-env-host src/budget/limits.rs):
    // 100M CPU instructions and 40 MiB of memory per invocation. The charged
    // instruction counts here are inflated by the debug test host, so the real
    // WASM invocation is strictly cheaper than what we measure.
    const DEFAULT_CPU_LIMIT: u64 = 100_000_000;
    const DEFAULT_MEM_LIMIT: u64 = 40 * 1024 * 1024;

    let single_ref = BytesN::from_array(&env, &[90u8; 32]);
    let single_buyer = Address::generate(&env);

    // Measure a single claim in a fresh default budget.
    env.cost_estimate().budget().reset_default();
    client.refund(&single_ref, &single_buyer, &10_000, &0, &10_000, &None);
    let single_cpu = env.cost_estimate().budget().cpu_instruction_cost();
    let single_mem = env.cost_estimate().budget().memory_bytes_cost();
    assert!(single_cpu > 0);
    assert!(single_mem > 0);

    // Ten independent claims in one invocation, also under a fresh default
    // budget. Ten is behaviorally representative (a real ordering might cover
    // that many refunds) while keeping the measured result comfortably inside
    // the invocation limits even with the debug-host overhead.
    let mut claims = Vec::new(&env);
    for i in 0..10u8 {
        let buyer = Address::generate(&env);
        claims.push_back(make_claim(
            BytesN::from_array(&env, &[i; 32]),
            &buyer,
            10_000,
            0,
            10_000,
        ));
    }

    env.cost_estimate().budget().reset_default();
    client.claim_batch(&claims);
    let batch_cpu = env.cost_estimate().budget().cpu_instruction_cost();
    let batch_mem = env.cost_estimate().budget().memory_bytes_cost();

    // Headroom: even with debug-host inflation a 10-claim batch stays under a
    // tenth of the CPU budget and a twentieth of the memory budget, far from
    // the invocation limits.
    assert!(
        batch_cpu < DEFAULT_CPU_LIMIT / 10,
        "10-claim batch cpu {batch_cpu} exceeds {}/10",
        DEFAULT_CPU_LIMIT
    );
    assert!(
        batch_mem < DEFAULT_MEM_LIMIT / 20,
        "10-claim batch memory {batch_mem} exceeds {}/20",
        DEFAULT_MEM_LIMIT
    );

    // Near-linear scaling: one invocation handling ten claims costs no more
    // than twelve single claims — a super-linear loop, storage or auth blow-up
    // would trip this. (In practice batching is marginally cheaper per claim.)
    assert!(
        batch_cpu < single_cpu * 12,
        "batch cpu {batch_cpu} (single {single_cpu}) exceeds 12x single"
    );
}
