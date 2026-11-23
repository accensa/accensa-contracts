//! Table-driven exhaustive integration test matrix verifying state channel phase transitions
//! and error codes (issue #1010).
//!
//! Asserts that every public contract entry point (`close_channel`, `dispute`,
//! `finalize_dispute`, `claim`, `reclaim`, `update_state`, `submit_counter_evidence`)
//! rejects out-of-order invocations from invalid channel phases (`Open`, `Closed`,
//! `Disputed`, `Finalized`) with exact typed `Error` variants.

#![cfg(test)]

extern crate std;

use accensa_common::Error;
use ed25519_dalek::{Signer, SigningKey, SECRET_KEY_LENGTH};
use soroban_sdk::{
    testutils::{Address as _, EnvTestConfig, Ledger},
    token::StellarAssetClient,
    Address, BytesN, Env,
};
use state_channel::{
    Channel, ChannelPhase, DataKey, StateChannel, StateChannelClient, StateUpdate,
};

/// Public entry points under test for phase verification.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EntryPoint {
    UpdateState,
    CloseChannel,
    Dispute,
    FinalizeDispute,
    Claim,
    Reclaim,
    SubmitCounterEvidence,
}

impl EntryPoint {
    pub const ALL: [EntryPoint; 7] = [
        EntryPoint::UpdateState,
        EntryPoint::CloseChannel,
        EntryPoint::Dispute,
        EntryPoint::FinalizeDispute,
        EntryPoint::Claim,
        EntryPoint::Reclaim,
        EntryPoint::SubmitCounterEvidence,
    ];

    pub fn name(&self) -> &'static str {
        match self {
            EntryPoint::UpdateState => "update_state",
            EntryPoint::CloseChannel => "close_channel",
            EntryPoint::Dispute => "dispute",
            EntryPoint::FinalizeDispute => "finalize_dispute",
            EntryPoint::Claim => "claim",
            EntryPoint::Reclaim => "reclaim",
            EntryPoint::SubmitCounterEvidence => "submit_counter_evidence",
        }
    }

    /// Returns the permitted lifecycle phase(s) for this entry point.
    pub fn is_phase_permitted(&self, phase: ChannelPhase) -> bool {
        match (self, phase) {
            (EntryPoint::UpdateState, ChannelPhase::Open) => true,
            (EntryPoint::CloseChannel, ChannelPhase::Open) => true,
            (EntryPoint::Dispute, ChannelPhase::Closed) => true,
            (EntryPoint::FinalizeDispute, ChannelPhase::Disputed) => true,
            (EntryPoint::Claim, ChannelPhase::Closed) => true,
            (EntryPoint::Reclaim, ChannelPhase::Open) => true,
            (EntryPoint::SubmitCounterEvidence, ChannelPhase::Disputed) => true,
            _ => false,
        }
    }

    /// Returns the exact typed error expected when invoking this entry point from an invalid phase.
    pub fn expected_phase_error(&self, phase: ChannelPhase) -> Option<Error> {
        if self.is_phase_permitted(phase) {
            None
        } else {
            match self {
                EntryPoint::UpdateState => Some(Error::ChannelNotOpen),
                EntryPoint::CloseChannel => Some(Error::ChannelNotOpen),
                EntryPoint::Dispute => Some(Error::ChannelNotOpen),
                EntryPoint::FinalizeDispute => Some(Error::ChannelNotOpen),
                EntryPoint::Claim => Some(Error::ChallengeActive),
                EntryPoint::Reclaim => Some(Error::ChannelAlreadyClosed),
                EntryPoint::SubmitCounterEvidence => Some(Error::ChannelNotOpen),
            }
        }
    }
}

pub const ALL_PHASES: [ChannelPhase; 4] = [
    ChannelPhase::Open,
    ChannelPhase::Closed,
    ChannelPhase::Disputed,
    ChannelPhase::Finalized,
];

/// Test harness fixture.
struct Fixture {
    env: Env,
    contract_id: Address,
    client: StateChannelClient<'static>,
    sender: Address,
    receiver: Address,
    sk: SigningKey,
    sender_pubkey: BytesN<32>,
}

fn setup() -> Fixture {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    env.ledger().set_sequence_number(100);

    let admin = Address::generate(&env);
    let sender = Address::generate(&env);
    let receiver = Address::generate(&env);

    let sac = env.register_stellar_asset_contract_v2(admin);
    let token = sac.address();

    let contract_id = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract_id);
    client.initialize(&token);

    StellarAssetClient::new(&env, &token).mint(&sender, &1_000_000_000i128);

    let sk = SigningKey::from_bytes(&[9u8; SECRET_KEY_LENGTH]);
    let vk = sk.verifying_key();
    let sender_pubkey = BytesN::from_array(&env, &vk.to_bytes());

    Fixture {
        env,
        contract_id,
        client,
        sender,
        receiver,
        sk,
        sender_pubkey,
    }
}

fn sign_state(
    env: &Env,
    sk: &SigningKey,
    sender_pubkey: &BytesN<32>,
    state: &StateUpdate,
) -> BytesN<64> {
    let mut msg = [0u8; 56];
    msg[..32].copy_from_slice(&sender_pubkey.to_array());
    msg[32..40].copy_from_slice(&state.nonce.to_be_bytes());
    msg[40..56].copy_from_slice(&state.balance.to_be_bytes());
    let sig = sk.sign(&msg);
    BytesN::from_array(env, &sig.to_bytes())
}

/// Creates a channel that evolved naturally into `target_phase` via standard contract calls.
fn create_channel_natural_phase(fx: &Fixture, target_phase: ChannelPhase) -> u64 {
    let challenge_period = 20u32;
    let channel_id = fx.client.open_channel(
        &fx.sender,
        &fx.receiver,
        &fx.sender_pubkey,
        &10_000i128,
        &challenge_period,
    );

    match target_phase {
        ChannelPhase::Open => channel_id,
        ChannelPhase::Closed => {
            let state = StateUpdate {
                nonce: 1,
                balance: 1_000,
            };
            let sig = sign_state(&fx.env, &fx.sk, &fx.sender_pubkey, &state);
            fx.client.close_channel(&channel_id, &state, &sig);
            assert_eq!(
                fx.client.get_channel(&channel_id).phase,
                ChannelPhase::Closed
            );
            channel_id
        }
        ChannelPhase::Disputed => {
            let state1 = StateUpdate {
                nonce: 1,
                balance: 1_000,
            };
            let sig1 = sign_state(&fx.env, &fx.sk, &fx.sender_pubkey, &state1);
            fx.client.close_channel(&channel_id, &state1, &sig1);

            let state2 = StateUpdate {
                nonce: 2,
                balance: 2_000,
            };
            let sig2 = sign_state(&fx.env, &fx.sk, &fx.sender_pubkey, &state2);
            fx.client.dispute(&channel_id, &state2, &sig2);
            assert_eq!(
                fx.client.get_channel(&channel_id).phase,
                ChannelPhase::Disputed
            );
            channel_id
        }
        ChannelPhase::Finalized => {
            let state = StateUpdate {
                nonce: 1,
                balance: 1_000,
            };
            let sig = sign_state(&fx.env, &fx.sk, &fx.sender_pubkey, &state);
            fx.client.close_channel(&channel_id, &state, &sig);
            let ch = fx.client.get_channel(&channel_id);
            fx.env
                .ledger()
                .set_sequence_number(ch.closed_at + challenge_period + 1);
            fx.client.claim(&channel_id);
            assert_eq!(
                fx.client.get_channel(&channel_id).phase,
                ChannelPhase::Finalized
            );
            channel_id
        }
    }
}

/// Creates a mock channel explicitly placed in `target_phase` via direct contract instance storage.
fn create_channel_mock_phase(fx: &Fixture, target_phase: ChannelPhase) -> u64 {
    let challenge_period = 20u32;
    let channel_id = fx.client.open_channel(
        &fx.sender,
        &fx.receiver,
        &fx.sender_pubkey,
        &10_000i128,
        &challenge_period,
    );

    if target_phase != ChannelPhase::Open {
        fx.env.as_contract(&fx.contract_id, || {
            let mut ch: Channel = fx
                .env
                .storage()
                .instance()
                .get(&DataKey::Channel(channel_id))
                .unwrap();
            ch.phase = target_phase;
            let current = fx.env.ledger().sequence();
            ch.closed_at = current;
            if target_phase == ChannelPhase::Disputed {
                ch.disputed_at = current;
            }
            fx.env
                .storage()
                .instance()
                .set(&DataKey::Channel(channel_id), &ch);
        });
    }

    assert_eq!(fx.client.get_channel(&channel_id).phase, target_phase);
    channel_id
}

/// Invokes an entry point using valid arguments and returns the result.
fn invoke_entry_point(fx: &Fixture, channel_id: u64, entry_point: EntryPoint) -> Result<(), Error> {
    let ch = fx.client.get_channel(&channel_id);
    let candidate_nonce = ch.nonce + 10;
    let candidate_balance = ch.balance.max(500);

    let state = StateUpdate {
        nonce: candidate_nonce,
        balance: candidate_balance,
    };
    let sig = sign_state(&fx.env, &fx.sk, &fx.sender_pubkey, &state);

    let res = match entry_point {
        EntryPoint::UpdateState => fx.client.try_update_state(&channel_id, &state, &sig),
        EntryPoint::CloseChannel => fx.client.try_close_channel(&channel_id, &state, &sig),
        EntryPoint::Dispute => fx.client.try_dispute(&channel_id, &state, &sig),
        EntryPoint::FinalizeDispute => fx.client.try_finalize_dispute(&channel_id),
        EntryPoint::Claim => fx.client.try_claim(&channel_id),
        EntryPoint::Reclaim => fx.client.try_reclaim(&channel_id),
        EntryPoint::SubmitCounterEvidence => {
            fx.client
                .try_submit_counter_evidence(&channel_id, &state, &sig)
        }
    };

    match res {
        Ok(Ok(())) => Ok(()),
        Err(Ok(err)) => Err(err),
        other => panic!("unexpected invoke outcome: {:?}", other),
    }
}

// ── Table-Driven Exhaustive Matrix Tests ─────────────────────────────────────

#[test]
fn test_exhaustive_phase_matrix_natural_channels() {
    let fx = setup();

    for &phase in &ALL_PHASES {
        for &entry in &EntryPoint::ALL {
            let channel_id = create_channel_natural_phase(&fx, phase);
            let expected_err = entry.expected_phase_error(phase);

            let res = invoke_entry_point(&fx, channel_id, entry);

            if let Some(err) = expected_err {
                assert_eq!(
                    res,
                    Err(err),
                    "Entry point '{}' on natural channel in phase '{:?}' must return error '{:?}', got '{:?}'",
                    entry.name(),
                    phase,
                    err,
                    res
                );
            } else {
                // When phase is permitted, the call should not fail with its invalid-phase rejection error.
                if entry == EntryPoint::Reclaim {
                    if let Err(actual_err) = res {
                        assert_ne!(
                            actual_err,
                            Error::ChannelAlreadyClosed,
                            "reclaim in permitted Open phase must not return ChannelAlreadyClosed"
                        );
                    }
                } else if entry != EntryPoint::Claim {
                    if let Err(actual_err) = res {
                        assert_ne!(
                            actual_err,
                            Error::ChannelNotOpen,
                            "Entry point '{}' in permitted phase '{:?}' should not fail with ChannelNotOpen",
                            entry.name(),
                            phase
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn test_exhaustive_phase_matrix_mock_channels() {
    let fx = setup();

    for &phase in &ALL_PHASES {
        for &entry in &EntryPoint::ALL {
            let channel_id = create_channel_mock_phase(&fx, phase);
            let expected_err = entry.expected_phase_error(phase);

            let res = invoke_entry_point(&fx, channel_id, entry);

            if let Some(err) = expected_err {
                assert_eq!(
                    res,
                    Err(err),
                    "Entry point '{}' on mock channel in phase '{:?}' must return error '{:?}', got '{:?}'",
                    entry.name(),
                    phase,
                    err,
                    res
                );
            }
        }
    }
}

// ── Granular Parameterized Tests Per Entry Point ────────────────────────────

#[test]
fn test_close_channel_phase_rejections() {
    let fx = setup();
    let state = StateUpdate {
        nonce: 1,
        balance: 100,
    };
    let sig = sign_state(&fx.env, &fx.sk, &fx.sender_pubkey, &state);

    // Permitted: Open. Invalid: Closed, Disputed, Finalized -> Error::ChannelNotOpen.
    let invalid_cases = [
        ChannelPhase::Closed,
        ChannelPhase::Disputed,
        ChannelPhase::Finalized,
    ];

    for &phase in &invalid_cases {
        let ch_id = create_channel_mock_phase(&fx, phase);
        assert_eq!(
            fx.client.try_close_channel(&ch_id, &state, &sig),
            Err(Ok(Error::ChannelNotOpen)),
            "close_channel must return ChannelNotOpen when called on phase {:?}",
            phase
        );
    }
}

#[test]
fn test_update_state_phase_rejections() {
    let fx = setup();
    let state = StateUpdate {
        nonce: 1,
        balance: 100,
    };
    let sig = sign_state(&fx.env, &fx.sk, &fx.sender_pubkey, &state);

    // Permitted: Open. Invalid: Closed, Disputed, Finalized -> Error::ChannelNotOpen.
    let invalid_cases = [
        ChannelPhase::Closed,
        ChannelPhase::Disputed,
        ChannelPhase::Finalized,
    ];

    for &phase in &invalid_cases {
        let ch_id = create_channel_mock_phase(&fx, phase);
        assert_eq!(
            fx.client.try_update_state(&ch_id, &state, &sig),
            Err(Ok(Error::ChannelNotOpen)),
            "update_state must return ChannelNotOpen when called on phase {:?}",
            phase
        );
    }
}

#[test]
fn test_dispute_phase_rejections() {
    let fx = setup();
    let state = StateUpdate {
        nonce: 2,
        balance: 200,
    };
    let sig = sign_state(&fx.env, &fx.sk, &fx.sender_pubkey, &state);

    // Permitted: Closed. Invalid: Open, Disputed, Finalized -> Error::ChannelNotOpen.
    let invalid_cases = [
        ChannelPhase::Open,
        ChannelPhase::Disputed,
        ChannelPhase::Finalized,
    ];

    for &phase in &invalid_cases {
        let ch_id = create_channel_mock_phase(&fx, phase);
        assert_eq!(
            fx.client.try_dispute(&ch_id, &state, &sig),
            Err(Ok(Error::ChannelNotOpen)),
            "dispute must return ChannelNotOpen when called on phase {:?}",
            phase
        );
    }
}

#[test]
fn test_finalize_dispute_phase_rejections() {
    let fx = setup();

    // Permitted: Disputed. Invalid: Open, Closed, Finalized -> Error::ChannelNotOpen.
    let invalid_cases = [
        ChannelPhase::Open,
        ChannelPhase::Closed,
        ChannelPhase::Finalized,
    ];

    for &phase in &invalid_cases {
        let ch_id = create_channel_mock_phase(&fx, phase);
        assert_eq!(
            fx.client.try_finalize_dispute(&ch_id),
            Err(Ok(Error::ChannelNotOpen)),
            "finalize_dispute must return ChannelNotOpen when called on phase {:?}",
            phase
        );
    }
}

#[test]
fn test_claim_phase_rejections() {
    let fx = setup();

    // Permitted: Closed. Invalid: Open, Disputed, Finalized -> Error::ChallengeActive.
    let invalid_cases = [
        ChannelPhase::Open,
        ChannelPhase::Disputed,
        ChannelPhase::Finalized,
    ];

    for &phase in &invalid_cases {
        let ch_id = create_channel_mock_phase(&fx, phase);
        assert_eq!(
            fx.client.try_claim(&ch_id),
            Err(Ok(Error::ChallengeActive)),
            "claim must return ChallengeActive when called on phase {:?}",
            phase
        );
    }
}

#[test]
fn test_reclaim_phase_rejections() {
    let fx = setup();

    // Permitted: Open. Invalid: Closed, Disputed, Finalized -> Error::ChannelAlreadyClosed.
    let invalid_cases = [
        ChannelPhase::Closed,
        ChannelPhase::Disputed,
        ChannelPhase::Finalized,
    ];

    for &phase in &invalid_cases {
        let ch_id = create_channel_mock_phase(&fx, phase);
        assert_eq!(
            fx.client.try_reclaim(&ch_id),
            Err(Ok(Error::ChannelAlreadyClosed)),
            "reclaim must return ChannelAlreadyClosed when called on phase {:?}",
            phase
        );
    }
}

#[test]
fn test_submit_counter_evidence_phase_rejections() {
    let fx = setup();
    let state = StateUpdate {
        nonce: 3,
        balance: 300,
    };
    let sig = sign_state(&fx.env, &fx.sk, &fx.sender_pubkey, &state);

    // Permitted: Disputed. Invalid: Open, Closed, Finalized -> Error::ChannelNotOpen.
    let invalid_cases = [
        ChannelPhase::Open,
        ChannelPhase::Closed,
        ChannelPhase::Finalized,
    ];

    for &phase in &invalid_cases {
        let ch_id = create_channel_mock_phase(&fx, phase);
        assert_eq!(
            fx.client.try_submit_counter_evidence(&ch_id, &state, &sig),
            Err(Ok(Error::ChannelNotOpen)),
            "submit_counter_evidence must return ChannelNotOpen when called on phase {:?}",
            phase
        );
    }
}

// ── State Machine Transition Invariants ─────────────────────────────────────

#[test]
fn test_full_lifecycle_and_transition_invariants() {
    let fx = setup();
    let challenge_period = 10u32;
    let ch_id = fx.client.open_channel(
        &fx.sender,
        &fx.receiver,
        &fx.sender_pubkey,
        &5_000i128,
        &challenge_period,
    );

    // Phase: Open
    assert_eq!(fx.client.get_channel(&ch_id).phase, ChannelPhase::Open);

    // 1. In Open phase, dispute/finalize/claim are rejected.
    let dummy_state = StateUpdate {
        nonce: 1,
        balance: 100,
    };
    let dummy_sig = sign_state(&fx.env, &fx.sk, &fx.sender_pubkey, &dummy_state);
    assert_eq!(
        fx.client.try_dispute(&ch_id, &dummy_state, &dummy_sig),
        Err(Ok(Error::ChannelNotOpen))
    );
    assert_eq!(
        fx.client.try_finalize_dispute(&ch_id),
        Err(Ok(Error::ChannelNotOpen))
    );
    assert_eq!(fx.client.try_claim(&ch_id), Err(Ok(Error::ChallengeActive)));

    // Transition Open -> Closed
    let close_state = StateUpdate {
        nonce: 1,
        balance: 1_000,
    };
    let close_sig = sign_state(&fx.env, &fx.sk, &fx.sender_pubkey, &close_state);
    assert_eq!(
        fx.client
            .try_close_channel(&ch_id, &close_state, &close_sig),
        Ok(Ok(()))
    );
    assert_eq!(fx.client.get_channel(&ch_id).phase, ChannelPhase::Closed);

    // 2. In Closed phase, update_state/close_channel/reclaim/finalize_dispute are rejected.
    assert_eq!(
        fx.client.try_update_state(&ch_id, &close_state, &close_sig),
        Err(Ok(Error::ChannelNotOpen))
    );
    assert_eq!(
        fx.client
            .try_close_channel(&ch_id, &close_state, &close_sig),
        Err(Ok(Error::ChannelNotOpen))
    );
    assert_eq!(
        fx.client.try_reclaim(&ch_id),
        Err(Ok(Error::ChannelAlreadyClosed))
    );
    assert_eq!(
        fx.client.try_finalize_dispute(&ch_id),
        Err(Ok(Error::ChannelNotOpen))
    );

    // Transition Closed -> Disputed
    let dispute_state = StateUpdate {
        nonce: 2,
        balance: 1_500,
    };
    let dispute_sig = sign_state(&fx.env, &fx.sk, &fx.sender_pubkey, &dispute_state);
    assert_eq!(
        fx.client.try_dispute(&ch_id, &dispute_state, &dispute_sig),
        Ok(Ok(()))
    );
    assert_eq!(fx.client.get_channel(&ch_id).phase, ChannelPhase::Disputed);

    // 3. In Disputed phase, update_state/close_channel/dispute/claim/reclaim are rejected.
    assert_eq!(
        fx.client
            .try_update_state(&ch_id, &dispute_state, &dispute_sig),
        Err(Ok(Error::ChannelNotOpen))
    );
    assert_eq!(
        fx.client
            .try_close_channel(&ch_id, &dispute_state, &dispute_sig),
        Err(Ok(Error::ChannelNotOpen))
    );
    assert_eq!(
        fx.client.try_dispute(&ch_id, &dispute_state, &dispute_sig),
        Err(Ok(Error::ChannelNotOpen))
    );
    assert_eq!(fx.client.try_claim(&ch_id), Err(Ok(Error::ChallengeActive)));
    assert_eq!(
        fx.client.try_reclaim(&ch_id),
        Err(Ok(Error::ChannelAlreadyClosed))
    );

    // Transition Disputed -> Finalized
    let ch = fx.client.get_channel(&ch_id);
    fx.env
        .ledger()
        .set_sequence_number(ch.disputed_at + challenge_period + 1);
    assert_eq!(fx.client.try_finalize_dispute(&ch_id), Ok(Ok(())));
    assert_eq!(fx.client.get_channel(&ch_id).phase, ChannelPhase::Finalized);

    // 4. In Finalized phase, ALL mutating entry points are unconditionally rejected.
    assert_eq!(
        fx.client
            .try_update_state(&ch_id, &dispute_state, &dispute_sig),
        Err(Ok(Error::ChannelNotOpen))
    );
    assert_eq!(
        fx.client
            .try_close_channel(&ch_id, &dispute_state, &dispute_sig),
        Err(Ok(Error::ChannelNotOpen))
    );
    assert_eq!(
        fx.client.try_dispute(&ch_id, &dispute_state, &dispute_sig),
        Err(Ok(Error::ChannelNotOpen))
    );
    assert_eq!(
        fx.client.try_finalize_dispute(&ch_id),
        Err(Ok(Error::ChannelNotOpen))
    );
    assert_eq!(fx.client.try_claim(&ch_id), Err(Ok(Error::ChallengeActive)));
    assert_eq!(
        fx.client.try_reclaim(&ch_id),
        Err(Ok(Error::ChannelAlreadyClosed))
    );
    assert_eq!(
        fx.client
            .try_submit_counter_evidence(&ch_id, &dispute_state, &dispute_sig),
        Err(Ok(Error::ChannelNotOpen))
    );
}

#[test]
fn test_valid_claim_transition_to_finalized() {
    let fx = setup();
    let challenge_period = 10u32;
    let ch_id = fx.client.open_channel(
        &fx.sender,
        &fx.receiver,
        &fx.sender_pubkey,
        &5_000i128,
        &challenge_period,
    );

    let state = StateUpdate {
        nonce: 1,
        balance: 1_000,
    };
    let sig = sign_state(&fx.env, &fx.sk, &fx.sender_pubkey, &state);
    fx.client.close_channel(&ch_id, &state, &sig);
    assert_eq!(fx.client.get_channel(&ch_id).phase, ChannelPhase::Closed);

    // Before challenge window expires, claim fails with ChallengeActive.
    assert_eq!(fx.client.try_claim(&ch_id), Err(Ok(Error::ChallengeActive)));

    // After challenge window expires, claim succeeds and finalizes the channel.
    let ch = fx.client.get_channel(&ch_id);
    fx.env
        .ledger()
        .set_sequence_number(ch.closed_at + challenge_period + 1);
    assert_eq!(fx.client.try_claim(&ch_id), Ok(Ok(())));
    assert_eq!(fx.client.get_channel(&ch_id).phase, ChannelPhase::Finalized);

    // Repeated claim on Finalized fails.
    assert_eq!(fx.client.try_claim(&ch_id), Err(Ok(Error::ChallengeActive)));
}

#[test]
fn test_valid_reclaim_transition_to_finalized() {
    let fx = setup();
    let challenge_period = 10u32;
    let ch_id = fx.client.open_channel(
        &fx.sender,
        &fx.receiver,
        &fx.sender_pubkey,
        &5_000i128,
        &challenge_period,
    );
    assert_eq!(fx.client.get_channel(&ch_id).phase, ChannelPhase::Open);

    // Before max lifetime expires, reclaim fails with ChannelNotOpen (meaning still actively open).
    assert_eq!(
        fx.client.try_reclaim(&ch_id),
        Err(Ok(Error::ChannelNotOpen))
    );

    // After max lifetime expires, reclaim succeeds and finalizes the channel.
    let ch = fx.client.get_channel(&ch_id);
    let max_lifetime = fx.client.get_max_channel_lifetime();
    fx.env
        .ledger()
        .set_sequence_number(ch.opened_at + max_lifetime + 1);
    assert_eq!(fx.client.try_reclaim(&ch_id), Ok(Ok(())));
    assert_eq!(fx.client.get_channel(&ch_id).phase, ChannelPhase::Finalized);

    // Repeated reclaim on Finalized fails with ChannelAlreadyClosed.
    assert_eq!(
        fx.client.try_reclaim(&ch_id),
        Err(Ok(Error::ChannelAlreadyClosed))
    );
}
