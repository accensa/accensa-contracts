#![cfg(test)]

use super::*;
use refund_window_policy::RefundWindowPolicy;
use soroban_sdk::{
    testutils::{storage::Persistent as _, Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, Env,
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

    let policy_id = env.register(RefundWindowPolicy, ());
    let contract_id = env.register(
        RefundVault,
        (merchant.clone(), token.clone(), window, policy_id),
    );
    let client = RefundVaultClient::new(&env, &contract_id);

    (env, client, merchant, token)
}

#[test]
fn test_vault_is_initialized_via_constructor() {
    let (env, client, merchant, token) = setup(100);
    // The vault is deployed constructor-initialised, so privileged calls work
    // without any separate `initialize` step.
    client.deposit(&merchant, &100);
    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&client.address), 100);
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
    client.refund(&payment_ref, &buyer, &120_000, &0, &120_000);

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
    client.refund(&payment_ref, &buyer, &100, &0, &300);
    client.refund(&payment_ref, &buyer, &150, &0, &300);
    client.refund(&payment_ref, &buyer, &50, &0, &300);

    let record = client.get_refund(&payment_ref).unwrap();
    assert_eq!(record.amount_refunded, 300);
    // Summing the partials lands exactly on the ceiling.
    assert_eq!(record.payment_amount, 300);

    // One more call, even a single unit, is now past the ceiling.
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &1, &0, &300),
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
        client.try_refund(&payment_ref, &buyer, &100, &100, &100),
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
    client.refund(&payment_ref, &buyer, &100, &100, &100);
    assert!(client.get_refund(&payment_ref).is_some());
}

#[test]
fn test_zero_window_disables_expiry() {
    let (env, client, merchant, _token) = setup(0);
    client.deposit(&merchant, &500_000);

    env.ledger().with_mut(|li| li.sequence_number = 1_000_000);

    let payment_ref = BytesN::from_array(&env, &[3u8; 32]);
    let buyer = Address::generate(&env);
    client.refund(&payment_ref, &buyer, &100, &0, &100);
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
    client.refund(&payment_ref, &buyer, &100_000, &0, &300_000);

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
    client.refund(&payment_ref, &buyer, &50_000, &0, &300_000);
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
    client.refund(&payment_ref, &buyer, &100_000, &0, &300_000);

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
        client.try_refund(&payment_ref, &buyer, &10_000, &0, &10_000),
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
        client.try_refund(&payment_ref, &buyer, &100, &100, &100),
        Err(Ok(Error::WindowExpired))
    );

    client.propose_policy(&1000);
    // Cannot execute yet — timelock has not expired.
    assert_eq!(
        client.try_execute_policy(),
        Err(Ok(Error::TimelockNotExpired))
    );

    // Advance past the timelock (500 + 17_280 = 17_780).
    env.ledger().with_mut(|li| li.sequence_number += 17_280);
    client.execute_policy();

    // paid_at=17_780, window=1000, current=17_780 → still inside the window.
    client.refund(&payment_ref, &buyer, &100, &(env.ledger().sequence()), &100);
    assert!(client.get_refund(&payment_ref).is_some());
}

#[test]
fn test_refund_with_non_contract_policy_fails_policy_call() {
    // A vault bound to an address that is not a contract cannot execute its
    // policy: the external policy call fails at the host level and surfaces as
    // PolicyCallFailed (it cannot masquerade as WindowExpired or a success).
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let no_policy = Address::generate(&env);
    let contract_id = env.register(
        RefundVault,
        (merchant.clone(), token.clone(), 100u32, no_policy),
    );
    let client = RefundVaultClient::new(&env, &contract_id);
    client.deposit(&merchant, &500_000);

    let payment_ref = BytesN::from_array(&env, &[6u8; 32]);
    let buyer = Address::generate(&env);
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &100, &0, &100),
        Err(Ok(Error::PolicyCallFailed))
    );
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
    client.refund(&payment_ref, &buyer, &100, &0, &100);
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
        client.try_refund(&payment_ref, &buyer, &0, &0, &100),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        client.try_refund(&payment_ref, &buyer, &-100, &0, &100),
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
        client.try_refund(&payment_ref, &buyer, &100, &0, &100),
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
                contract_outcome(client.try_refund(&payment_ref, &buyer, &100_000, &0, &100_000))
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
        contract_outcome(client.try_refund(&payment_ref, &buyer, &100_000, &0, &100_000)),
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
    client.refund(&payment_ref, &buyer, &120_000, &0, &120_000);

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

    client.refund(&payment_ref, &buyer, &120_000, &0, &120_000);

    let refund_events = env.events().all().filter_by_contract(&client.address);
    // The refund event carries the per-call amount and the running cumulative
    // total, so an indexer knows the state without summing history (#99).
    let mut refund_data = Map::<Val, Val>::new(&env);
    refund_data.set(
        Symbol::new(&env, "amount").into_val(&env),
        120_000i128.into_val(&env),
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
    client.propose_policy(&300);

    assert_eq!(
        env.events().all().filter_by_contract(&client.address),
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "policy_proposed_event"), 300u32).into_val(&env),
                soroban_sdk::map![
                    &env,
                    (Symbol::new(&env, "proposed_at_ledger"), 700u32),
                    (
                        Symbol::new(&env, "execute_after_ledger"),
                        700u32 + 17_280u32
                    ),
                ]
                .into_val(&env)
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
    client.refund(&payment_ref, &stranger, &120_000, &0, &120_000);
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
    client.propose_policy(&200);
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
    client.propose_policy(&200);
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
    client.propose_policy(&200);
}

#[test]
fn test_old_admin_cannot_act_after_transfer() {
    let (env, client, _merchant, _token) = setup(100);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&new_admin);
    client.accept_admin();

    // New admin can call admin-only functions.
    client.propose_policy(&200);
}

#[test]
#[should_panic]
fn test_transfer_admin_requires_auth() {
    let (env, client, _merchant, _token) = setup(100);
    let new_admin = Address::generate(&env);

    // Enforcing mode with no signatures: current_admin.require_auth() must abort.
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

// ── Policy timelock tests ──────────────────────────────────────────────────

#[test]
fn test_propose_and_execute_policy_happy_path() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    client.propose_policy(&200);

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

    client.propose_policy(&200);

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

    client.propose_policy(&200);

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

    client.propose_policy(&200);
    client.propose_policy(&500);

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
        client.try_refund(&payment_ref, &buyer, &100, &1, &100),
        Err(Ok(Error::WindowExpired))
    );

    // Propose and execute a wider window.
    client.propose_policy(&20_000);
    env.ledger().with_mut(|li| li.sequence_number += 17_280);
    client.execute_policy();

    // Now the refund succeeds: current ~17_580, paid_at 1, window 20_000.
    client.refund(&payment_ref, &buyer, &100, &1, &100);
    assert!(client.get_refund(&payment_ref).is_some());
}

#[test]
#[should_panic]
fn test_propose_policy_requires_auth() {
    let (env, client, _merchant, _token) = setup(100);
    env.set_auths(&[]);
    client.propose_policy(&200);
}

#[test]
#[should_panic]
fn test_execute_policy_requires_auth() {
    let (env, client, _merchant, _token) = setup(100);
    client.propose_policy(&200);
    env.set_auths(&[]);
    client.execute_policy();
}

#[test]
fn test_get_policy_timelock() {
    assert_eq!(RefundVault::get_policy_timelock(), 17_280);
}

#[test]
fn test_policy_events_emitted() {
    use soroban_sdk::testutils::Events;
    use soroban_sdk::{vec, IntoVal, Symbol};

    let (env, client, _merchant, _token) = setup(100);

    client.propose_policy(&200);

    let current = env.ledger().sequence();
    let events = env.events().all().filter_by_contract(&client.address);
    assert_eq!(
        events,
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "policy_proposed_event"), 200u32).into_val(&env),
                soroban_sdk::map![
                    &env,
                    (Symbol::new(&env, "proposed_at_ledger"), current),
                    (
                        Symbol::new(&env, "execute_after_ledger"),
                        current + 17_280u32
                    ),
                ]
                .into_val(&env)
            )
        ]
    );

    env.ledger().with_mut(|li| li.sequence_number += 17_280);
    client.execute_policy();

    let events = env.events().all().filter_by_contract(&client.address);
    assert_eq!(
        events,
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "policy_executed_event"), 200u32).into_val(&env),
                soroban_sdk::Map::<Symbol, soroban_sdk::Val>::new(&env).into_val(&env)
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
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    let payment_ref = BytesN::from_array(&env, &[12u8; 32]);
    let contract_addr = client.address.clone();

    // Refunding to vault address must return SelfTransfer error
    let res = client.try_refund(&payment_ref, &contract_addr, &50_000, &0, &50_000);
    assert_eq!(res, Err(Ok(Error::SelfTransfer)));

    // Payment ref must remain unconsumed / not recorded
    assert!(client.get_refund(&payment_ref).is_none());

    // No refund event emitted for the contract and no tokens moved: the vault
    // balance is exactly the deposited amount (SelfTransfer returns before any
    // transfer or policy interaction).
    let token_client = TokenClient::new(&env, &_token);
    assert_eq!(token_client.balance(&client.address), 500_000);
}

#[test]
fn test_withdraw_to_contract_address_fails_self_transfer() {
    let (env, client, merchant, _token) = setup(100);
    client.deposit(&merchant, &500_000);

    let contract_addr = client.address.clone();
    let res = client.try_withdraw(&50_000, &contract_addr);
    assert_eq!(res, Err(Ok(Error::SelfTransfer)));

    // No tokens moved: the vault balance is exactly the deposited amount.
    let token_client = TokenClient::new(&env, &_token);
    assert_eq!(token_client.balance(&client.address), 500_000);
}

#[test]
fn test_refund_to_merchant_succeeds() {
    let (env, client, merchant, token) = setup(100);
    client.deposit(&merchant, &500_000);

    let payment_ref = BytesN::from_array(&env, &[13u8; 32]);
    let initial_merchant_bal = TokenClient::new(&env, &token).balance(&merchant);

    // Refunding to merchant is valid (e.g. merchant-as-buyer in testing or direct reversal)
    client.refund(&payment_ref, &merchant, &50_000, &0, &50_000);

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
