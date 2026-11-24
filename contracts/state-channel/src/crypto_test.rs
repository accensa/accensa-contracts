//! Batched Ed25519 verification tests (issue #471).
//!
//! Covers the three behaviours the batch helper is responsible for: a fully
//! valid batch verifies, a length mismatch is rejected as a caller error
//! without touching the host, and a mixed batch containing one forged
//! signature aborts (the host traps) rather than silently accepting the valid
//! half.

extern crate std;

use accensa_common::Error;
use ed25519_dalek::{Signer, SigningKey};
use soroban_sdk::{Bytes, BytesN, Env};

use crate::crypto::verify_signatures;

/// The canonical message the batch is verified against.
const MESSAGE: &[u8] = b"accensa-batch-v1";

fn pubkey(env: &Env, sk: &SigningKey) -> BytesN<32> {
    BytesN::from_array(env, &sk.verifying_key().to_bytes())
}

fn sig(env: &Env, sk: &SigningKey, message: &[u8]) -> BytesN<64> {
    BytesN::from_array(env, &sk.sign(message).to_bytes())
}

#[test]
fn a_fully_valid_batch_verifies() {
    let env = Env::default();
    let payload = Bytes::from_slice(&env, MESSAGE);

    let a = SigningKey::from_bytes(&[1u8; 32]);
    let b = SigningKey::from_bytes(&[2u8; 32]);
    let c = SigningKey::from_bytes(&[3u8; 32]);

    let signers = [pubkey(&env, &a), pubkey(&env, &b), pubkey(&env, &c)];
    let signatures = [
        sig(&env, &a, MESSAGE),
        sig(&env, &b, MESSAGE),
        sig(&env, &c, MESSAGE),
    ];

    assert_eq!(
        verify_signatures(&env, &payload, &signers, &signatures),
        Ok(())
    );
}

#[test]
fn length_mismatch_is_rejected_without_touching_the_host() {
    let env = Env::default();
    let payload = Bytes::from_slice(&env, MESSAGE);

    let a = SigningKey::from_bytes(&[1u8; 32]);
    let b = SigningKey::from_bytes(&[2u8; 32]);

    // Two signers, one signature: rejected before any verification runs. Were
    // the check missing, `zip` would silently drop the spare signer and the
    // batch would "verify" while skipping a participant.
    let signers = [pubkey(&env, &a), pubkey(&env, &b)];
    let signatures = [sig(&env, &a, MESSAGE)];

    assert_eq!(
        verify_signatures(&env, &payload, &signers, &signatures),
        Err(Error::InvalidSignature)
    );
}

#[test]
#[should_panic]
fn one_forged_signature_aborts_the_whole_batch() {
    let env = Env::default();
    let payload = Bytes::from_slice(&env, MESSAGE);

    let a = SigningKey::from_bytes(&[1u8; 32]);
    let b = SigningKey::from_bytes(&[2u8; 32]);

    let signers = [pubkey(&env, &a), pubkey(&env, &b)];
    let signatures = [
        // Valid: signed the right message.
        sig(&env, &a, MESSAGE),
        // Forged: `b`'s key over a different message, so it must not verify.
        sig(&env, &b, b"accensa-batch-v1-forged"),
    ];

    let _ = verify_signatures(&env, &payload, &signers, &signatures);
}

// ── Watchtower and dispute window coverage ──────────────────────────────────

#[test]
fn test_take_reward_coverage() {
    use crate::watchtower::{record, take_reward};
    use crate::StateChannel;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::Address;

    let env = Env::default();
    let contract_id = env.register(StateChannel, ());
    let channel_id = 42u64;
    let watchtower = Address::generate(&env);

    env.as_contract(&contract_id, || {
        // 1. Unset bounty / no stored state returns zero reward
        assert_eq!(take_reward(&env, channel_id, 1_000), Ok((0, 1_000, None)));

        // 2. Bounty set with no watchtower returns zero reward
        env.storage().instance().set(
            &crate::DataKey::Bounty(channel_id),
            &crate::watchtower::WatchtowerBounty {
                reward_bps: 500,
                watchtower: None,
            },
        );
        assert_eq!(take_reward(&env, channel_id, 1_000), Ok((0, 1_000, None)));

        // 3. Record watchtower defender
        record(&env, channel_id, watchtower.clone());

        // 4. Split payout: 500 bps = 5% of 10,000 = 500 reward, remaining = 9,500
        assert_eq!(
            take_reward(&env, channel_id, 10_000),
            Ok((500, 9_500, Some(watchtower.clone())))
        );

        // 5. One-shot verification: watchtower is cleared after take_reward
        assert_eq!(take_reward(&env, channel_id, 10_000), Ok((0, 10_000, None)));

        // 6. Zero reward bps clears watchtower and returns zero reward
        env.storage().instance().set(
            &crate::DataKey::Bounty(channel_id + 1),
            &crate::watchtower::WatchtowerBounty {
                reward_bps: 0,
                watchtower: Some(watchtower.clone()),
            },
        );
        assert_eq!(
            take_reward(&env, channel_id + 1, 1_000),
            Ok((0, 1_000, None))
        );

        // 7. Non-positive receiver payout clears watchtower and returns zero reward
        env.storage().instance().set(
            &crate::DataKey::Bounty(channel_id + 2),
            &crate::watchtower::WatchtowerBounty {
                reward_bps: 500,
                watchtower: Some(watchtower.clone()),
            },
        );
        assert_eq!(take_reward(&env, channel_id + 2, 0), Ok((0, 0, None)));
        record(&env, channel_id + 2, watchtower.clone());
        assert_eq!(take_reward(&env, channel_id + 2, -100), Ok((0, -100, None)));

        // 8. Math overflow on multiplication returns MathOverflow error
        env.storage().instance().set(
            &crate::DataKey::Bounty(channel_id + 3),
            &crate::watchtower::WatchtowerBounty {
                reward_bps: 500,
                watchtower: Some(watchtower),
            },
        );
        assert_eq!(
            take_reward(&env, channel_id + 3, i128::MAX),
            Err(Error::MathOverflow)
        );
    });
}

#[test]
fn test_ensure_window_open_and_elapsed() {
    use crate::dispute::{ensure_window_elapsed, ensure_window_open};
    use soroban_sdk::testutils::Ledger;

    let env = Env::default();
    env.ledger().with_mut(|li| {
        li.sequence_number = 100;
    });

    let window_started = 50u32;
    let challenge_period = 50u32;
    // Deadline = 50 + 50 = 100

    // When current_ledger (100) <= deadline (100):
    // Window is still open
    assert_eq!(
        ensure_window_open(&env, window_started, challenge_period),
        Ok(())
    );
    // Window has not elapsed yet (fails with ChallengeActive)
    assert_eq!(
        ensure_window_elapsed(&env, window_started, challenge_period),
        Err(Error::ChallengeActive)
    );

    // Advance ledger past deadline: sequence_number = 101 > 100
    env.ledger().with_mut(|li| {
        li.sequence_number = 101;
    });

    // Window is now expired (fails with ChallengeExpired)
    assert_eq!(
        ensure_window_open(&env, window_started, challenge_period),
        Err(Error::ChallengeExpired)
    );
    // Window has now elapsed
    assert_eq!(
        ensure_window_elapsed(&env, window_started, challenge_period),
        Ok(())
    );
}
