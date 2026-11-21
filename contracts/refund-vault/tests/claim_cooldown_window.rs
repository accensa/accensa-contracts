//! #964 — the claim cooldown window has an exact boundary and a vault-wide scope.
//!
//! `src/test.rs` sets a 100-second cooldown and then advances the clock by 200
//! seconds, so it proves only that "long enough" is long enough: neither the
//! second at which the gate opens nor *whose* clock it is has ever been
//! asserted. These tests pin both.
//!
//! The gate reads a single `DataKey::LastClaim` timestamp for the whole vault
//! (see the comment at `lib.rs:883-886`, which names `UserLastClaim(Address)`
//! as the unimplemented per-recipient alternative). The second test therefore
//! asserts the *current* behavior — a different recipient inside an open window
//! is still rejected — rather than the behavior the setter's doc comment
//! describes. That mismatch is documentation, not storage layout; nothing here
//! changes either.

use accensa_common::{Error, VaultInit};
use refund_policy_time::TimePolicy;
use refund_policy_vdf::VdfPolicy;
use refund_vault::{RefundVault, RefundVaultClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env,
};

const FLOAT: i128 = 1_000_000;
const COOLDOWN: u64 = 100;
/// Ledger-count refund window. Wide enough that no assertion here is stopped
/// by the time gate instead of the cooldown gate, and 0 ledgers of elapsed
/// time means `paid_at_ledger = 0` is always inside it.
const WINDOW: u32 = 17_280;

fn setup() -> (
    Env,
    RefundVaultClient<'static>,
    Address,
    Address,
    TokenClient<'static>,
) {
    let env = Env::default();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let vault_id = env.register(
        RefundVault,
        (VaultInit {
            merchant: merchant.clone(),
            token: token.clone(),
            time_policy: Some(env.register(TimePolicy, ())),
            vdf_policy: Some(env.register(VdfPolicy, ())),
            fee_bps: 0,
            fee_recipient: None,
            refund_window: WINDOW,
            deadline: 0,
            vdf_delay: 0,
        },),
    );
    let client = RefundVaultClient::new(&env, &vault_id);

    // Every claim below pays 100 units, so 4_000 units of float covers all of
    // them with room to spare: the cooldown gate, not `InsufficientFloat`, is
    // what the rejections can only be explained by.
    client.deposit(&merchant, &(FLOAT / 4), &None);
    client.set_claim_cooldown(&COOLDOWN);

    let token_client = TokenClient::new(&env, &token);
    (env, client, merchant, token, token_client)
}

fn set_time(env: &Env, timestamp: u64) {
    env.ledger().with_mut(|li| li.timestamp = timestamp);
}

fn ref_at(env: &Env, byte: u8) -> BytesN<32> {
    BytesN::from_array(env, &[byte; 32])
}

/// The window opens on the second the cooldown ends, not after it.
///
/// `lib.rs:899` rejects while `now < last + cooldown`, so 1,099 is the last
/// blocked second and 1,100 is the first allowed one. A one-off in that
/// comparison (`<=`, or a `+1` on the threshold) is invisible to a test that
/// only jumps 200 seconds ahead.
#[test]
fn test_cooldown_boundary_blocks_1099_and_opens_at_1100() {
    let (env, client, merchant, _token, tc) = setup();
    let buyer_a = Address::generate(&env);
    let buyer_b = Address::generate(&env);
    // `deposit` in `setup` has already bumped the vault's global counter, so it
    // is measured from here rather than from zero. The per-merchant nonce is the
    // one `refund` replays against and `deposit` leaves alone, so it starts at 0.
    let vault_nonce = client.get_nonce();

    set_time(&env, 1_000);
    client.refund(&ref_at(&env, 1), &buyer_a, &100, &0, &100, &None, &0);
    assert_eq!(tc.balance(&buyer_a), 100);
    // The first claim consumed the merchant's nonce 0 and bumped both counters
    // once, so `1` is what the next call must present.
    assert_eq!(client.get_nonce(), vault_nonce + 1);
    assert_eq!(client.get_user_nonce(&merchant), 1);

    // One second short of the window: rejected, and nothing moved.
    set_time(&env, 1_099);
    assert_eq!(
        client.try_refund(&ref_at(&env, 2), &buyer_b, &100, &0, &100, &None, &1),
        Err(Ok(Error::ClaimCooldownNotElapsed))
    );
    assert_eq!(tc.balance(&buyer_b), 0);
    // The rejected call rolled back, so the nonce the vault expects next is
    // still `1` — the retry reuses it rather than skipping a number.
    assert_eq!(client.get_nonce(), vault_nonce + 1);
    assert_eq!(client.get_user_nonce(&merchant), 1);

    // Exactly `last + cooldown`: the gate is `now < last + cooldown`, so this
    // is the first second that passes.
    set_time(&env, 1_100);
    client.refund(&ref_at(&env, 2), &buyer_b, &100, &0, &100, &None, &1);
    assert_eq!(tc.balance(&buyer_b), 100);
    assert_eq!(client.get_nonce(), vault_nonce + 2);
    assert_eq!(client.get_user_nonce(&merchant), 2);
}

/// The window belongs to the vault, not to the recipient that opened it.
///
/// This is the case the `set_claim_cooldown` doc comment ("for the same
/// recipient") gets wrong: a *different* buyer, claiming against a *different*
/// payment ref, is still rejected while the single `DataKey::LastClaim`
/// timestamp is inside the window.
#[test]
fn test_cooldown_is_vault_global_not_per_recipient() {
    let (env, client, _merchant, _token, tc) = setup();
    let buyer_a = Address::generate(&env);
    let buyer_b = Address::generate(&env);

    // Claim at 1,000, then claim again as soon as the window opens at 1,100 so
    // the recorded `LastClaim` is 1,100 rather than 1,000.
    set_time(&env, 1_000);
    client.refund(&ref_at(&env, 11), &buyer_a, &100, &0, &100, &None, &0);
    set_time(&env, 1_100);
    client.refund(&ref_at(&env, 12), &buyer_a, &100, &0, &100, &None, &1);

    // 1,150 is 50 seconds after the *second* claim, and `buyer_b` has never
    // claimed from this vault. Both are irrelevant to a vault-global gate.
    set_time(&env, 1_150);
    assert_eq!(
        client.try_refund(&ref_at(&env, 13), &buyer_b, &100, &0, &100, &None, &2),
        Err(Ok(Error::ClaimCooldownNotElapsed))
    );
    assert_eq!(tc.balance(&buyer_b), 0);

    // Same recipient, same ref, same instant: also blocked, which is the
    // partial-refund case the gate exists to slow down.
    assert_eq!(
        client.try_refund(&ref_at(&env, 12), &buyer_a, &50, &0, &200, &None, &2),
        Err(Ok(Error::ClaimCooldownNotElapsed))
    );

    // And it does open: 1,200 is the first second at or after 1,100 + 100.
    set_time(&env, 1_200);
    client.refund(&ref_at(&env, 13), &buyer_b, &100, &0, &100, &None, &2);
    assert_eq!(tc.balance(&buyer_b), 100);
}

/// `0` disables the gate entirely: back-to-back claims in the same second.
#[test]
fn test_zero_cooldown_allows_immediate_successive_claims() {
    let (env, client, merchant, _token, tc) = setup();
    let buyer_a = Address::generate(&env);
    let buyer_b = Address::generate(&env);

    set_time(&env, 1_000);
    let vault_nonce = client.get_nonce();
    client.refund(&ref_at(&env, 21), &buyer_a, &100, &0, &100, &None, &0);
    assert_eq!(
        client.try_refund(&ref_at(&env, 22), &buyer_b, &100, &0, &100, &None, &1),
        Err(Ok(Error::ClaimCooldownNotElapsed))
    );

    // Same instant, cooldown cleared: `lib.rs:890` skips the whole gate rather
    // than comparing against a zero window, so no clock advance is needed.
    client.set_claim_cooldown(&0);
    assert_eq!(client.get_claim_cooldown(), 0);
    client.refund(&ref_at(&env, 22), &buyer_b, &100, &0, &100, &None, &1);
    assert_eq!(tc.balance(&buyer_b), 100);
    // Two refunds so far; `set_claim_cooldown` is admin configuration and does
    // not take part in the merchant's replay counter.
    assert_eq!(client.get_nonce(), vault_nonce + 2);
    assert_eq!(client.get_user_nonce(&merchant), 2);

    // A third claim in that same second still goes through.
    client.refund(&ref_at(&env, 23), &buyer_a, &100, &0, &200, &None, &2);
    assert_eq!(tc.balance(&buyer_a), 200);
}

/// The gate is wall-clock, so a ledger jump without elapsed time does not open
/// it — and a time jump without ledgers does.
#[test]
fn test_cooldown_follows_timestamp_not_ledger_sequence() {
    let (env, client, _merchant, _token, tc) = setup();
    let buyer_a = Address::generate(&env);
    let buyer_b = Address::generate(&env);

    set_time(&env, 1_000);
    client.refund(&ref_at(&env, 31), &buyer_a, &100, &0, &100, &None, &0);

    // 10,000 ledgers pass, the timestamp does not move.
    env.ledger().with_mut(|li| li.sequence_number += 10_000);
    assert_eq!(
        client.try_refund(&ref_at(&env, 32), &buyer_b, &100, &0, &100, &None, &1),
        Err(Ok(Error::ClaimCooldownNotElapsed))
    );

    // The timestamp moves by one second short of the window, ledgers unchanged.
    set_time(&env, 1_099);
    assert_eq!(
        client.try_refund(&ref_at(&env, 32), &buyer_b, &100, &0, &100, &None, &1),
        Err(Ok(Error::ClaimCooldownNotElapsed))
    );

    set_time(&env, 1_100);
    client.refund(&ref_at(&env, 32), &buyer_b, &100, &0, &100, &None, &1);
    assert_eq!(tc.balance(&buyer_b), 100);
}
