//! Dispute, counter-evidence, finalization and claim event-shape tests
//! (issue #636).

extern crate std;

use super::*;
use ed25519_dalek::{Signer, SigningKey};
use soroban_sdk::{
    testutils::{Address as _, Events as _, Ledger},
    token::StellarAssetClient,
    Address, BytesN, Env, Event,
};

const DEPOSIT: i128 = 1_000;
const CHALLENGE: u32 = 100;
/// Ledgers a late counter-proof pushes the dispute window forward (issue #431).
const LATE_COUNTER_PROOF_EXTENSION: u32 = 50;

struct Setup {
    env: Env,
    client: StateChannelClient<'static>,
    contract: Address,
    sender: Address,
    receiver: Address,
    sender_sk: SigningKey,
}

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let sender = Address::generate(&env);
    let receiver = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    StellarAssetClient::new(&env, &token).mint(&sender, &DEPOSIT);

    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&token);

    Setup {
        env,
        client,
        contract,
        sender,
        receiver,
        sender_sk: SigningKey::from_bytes(&[5u8; 32]),
    }
}

impl Setup {
    fn open(&self) -> u64 {
        let pk = BytesN::from_array(&self.env, &self.sender_sk.verifying_key().to_bytes());
        self.client
            .open_channel(&self.sender, &self.receiver, &pk, &DEPOSIT, &CHALLENGE)
    }

    fn state(&self, nonce: u64, balance: i128) -> StateUpdate {
        StateUpdate { nonce, balance }
    }

    /// Sender-signed state, as `close_test::Setup::sign_update` builds it.
    fn sign(&self, state: &StateUpdate) -> BytesN<64> {
        let pk = BytesN::from_array(&self.env, &self.sender_sk.verifying_key().to_bytes());
        let mut msg = [0u8; 56];
        msg[..32].copy_from_slice(&pk.to_array());
        msg[32..40].copy_from_slice(&state.nonce.to_be_bytes());
        msg[40..56].copy_from_slice(&state.balance.to_be_bytes());
        BytesN::from_array(&self.env, &self.sender_sk.sign(&msg).to_bytes())
    }

    fn advance_to(&self, sequence: u32) {
        self.env.ledger().set_sequence_number(sequence);
    }

    /// Assert the last contract invocation published exactly `expected` for
    /// this contract — topic tuple and payload — compared by XDR, the same
    /// style as `hashlock_test.rs`. A failed invocation publishes nothing, so
    /// the expected vector is empty there.
    fn assert_last_events(&self, expected: std::vec::Vec<soroban_sdk::xdr::ContractEvent>) {
        assert_eq!(
            self.env.events().all().filter_by_contract(&self.contract),
            expected
        );
    }

    fn last_events(&self) -> std::vec::Vec<soroban_sdk::xdr::ContractEvent> {
        self.env
            .events()
            .all()
            .filter_by_contract(&self.contract)
            .events()
            .to_vec()
    }
}

/// dispute -> submit_counter_evidence -> finalize_dispute on one channel: each
/// call's event is pinned by exact XDR shape, and the settlement follows the
/// counter-proof state the channel actually ended in, not the state that was
/// first disputed.
#[test]
fn dispute_counter_evidence_and_finalize_publish_exact_event_shapes() {
    let s = setup();
    let id = s.open();

    // The sender closes with an early, low state.
    let close_state = s.state(1, 100);
    s.client
        .close_channel(&id, &close_state, &s.sign(&close_state));

    // The receiver disputes with a newer state: DisputeEvent carries that
    // state and the ledger the window was armed from.
    let disputed_at = s.env.ledger().sequence();
    let disputed = s.state(2, 300);
    s.client.dispute(&id, &disputed, &s.sign(&disputed));
    s.assert_last_events(std::vec![DisputeEvent {
        channel_id: id,
        nonce: 2,
        balance: 300,
        disputed_at,
    }
    .to_xdr(&s.env, &s.contract)]);

    // Counter-evidence: StateUpdatedEvent carries the counter-proof state.
    let counter_proof = s.state(3, 600);
    s.client
        .submit_counter_evidence(&id, &counter_proof, &s.sign(&counter_proof));
    s.assert_last_events(std::vec![StateUpdatedEvent {
        channel_id: id,
        nonce: 3,
        balance: 600,
    }
    .to_xdr(&s.env, &s.contract)]);
    // The recorded balance is the counter-proof state, not the disputed one.
    assert_eq!(s.client.get_channel(&id).balance, 600);

    // Finalize once the re-armed window elapses: the payout follows the
    // recorded (counter-proof) balance exactly.
    let ch = s.client.get_channel(&id);
    s.advance_to(ch.disputed_at + ch.challenge_period + 1);
    s.client.finalize_dispute(&id);
    s.assert_last_events(std::vec![DisputeFinalizedEvent {
        channel_id: id,
        receiver_payout: 600,
        sender_refund: DEPOSIT - 600,
    }
    .to_xdr(&s.env, &s.contract)]);
    assert_eq!(s.client.get_channel(&id).balance, 600);
}

/// `claim` on a channel that stayed Closed past its challenge window: the
/// ClaimEvent names the channel, the amount paid and the receiver paid.
#[test]
fn claim_publishes_the_exact_claim_event_shape() {
    let s = setup();
    let id = s.open();
    let close_state = s.state(1, 400);
    s.client
        .close_channel(&id, &close_state, &s.sign(&close_state));

    let ch = s.client.get_channel(&id);
    s.advance_to(ch.closed_at + ch.challenge_period + 1);
    s.client.claim(&id);
    s.assert_last_events(std::vec![ClaimEvent {
        channel_id: id,
        amount: 400,
        recipient: s.receiver.clone(),
    }
    .to_xdr(&s.env, &s.contract)]);
}

/// A counter-proof landing in the final 50 ledgers extends the window
/// (issue #431). Finalization must then be gated on the EXTENDED deadline:
/// rejected at the original one with no event, and publishing
/// DisputeFinalizedEvent only once the extended window has elapsed.
#[test]
fn late_counter_proof_extends_the_window_and_gates_the_finalized_event() {
    let s = setup();
    let id = s.open();
    let close_state = s.state(1, 100);
    s.client
        .close_channel(&id, &close_state, &s.sign(&close_state));

    let disputed_at = s.env.ledger().sequence();
    let disputed = s.state(2, 200);
    s.client.dispute(&id, &disputed, &s.sign(&disputed));
    let original_deadline = disputed_at + CHALLENGE;

    // Land inside the final stretch of the window (40 ledgers to go).
    s.advance_to(original_deadline - 40);
    let counter_proof = s.state(3, 300);
    s.client
        .submit_counter_evidence(&id, &counter_proof, &s.sign(&counter_proof));
    s.assert_last_events(std::vec![StateUpdatedEvent {
        channel_id: id,
        nonce: 3,
        balance: 300,
    }
    .to_xdr(&s.env, &s.contract)]);

    // The window itself moved: it now starts `LATE_COUNTER_PROOF_EXTENSION`
    // ledgers after the original dispute, one extension used.
    let ch = s.client.get_channel(&id);
    assert_eq!(ch.disputed_at, disputed_at + LATE_COUNTER_PROOF_EXTENSION);
    assert_eq!(ch.dispute_extensions, 1);

    // At the ORIGINAL deadline finalization is still rejected — and a
    // rejected call publishes no events at all.
    s.advance_to(original_deadline);
    assert_eq!(
        s.client.try_finalize_dispute(&id),
        Err(Ok(Error::ChallengeActive))
    );
    let rejected = DisputeFinalizedEvent {
        channel_id: id,
        receiver_payout: 300,
        sender_refund: DEPOSIT - 300,
    }
    .to_xdr(&s.env, &s.contract);
    let events = s.last_events();
    assert_eq!(
        events.len(),
        0,
        "a rejected finalization must publish no events"
    );
    assert!(events.iter().all(|e| e != &rejected));

    // Past the EXTENDED deadline the same call settles per the counter-proof.
    s.advance_to(disputed_at + LATE_COUNTER_PROOF_EXTENSION + CHALLENGE + 1);
    s.client.finalize_dispute(&id);
    s.assert_last_events(std::vec![DisputeFinalizedEvent {
        channel_id: id,
        receiver_payout: 300,
        sender_refund: DEPOSIT - 300,
    }
    .to_xdr(&s.env, &s.contract)]);
}

/// A finalization attempted while the dispute window is still open is
/// rejected with `ChallengeActive` and the events list is unchanged: no
/// DisputeFinalizedEvent (and nothing else) is published.
#[test]
fn finalize_dispute_before_the_window_elapses_publishes_no_dispute_finalized_event() {
    let s = setup();
    let id = s.open();
    let close_state = s.state(1, 100);
    s.client
        .close_channel(&id, &close_state, &s.sign(&close_state));

    let disputed_at = s.env.ledger().sequence();
    let disputed = s.state(2, 300);
    s.client.dispute(&id, &disputed, &s.sign(&disputed));
    s.assert_last_events(std::vec![DisputeEvent {
        channel_id: id,
        nonce: 2,
        balance: 300,
        disputed_at,
    }
    .to_xdr(&s.env, &s.contract)]);

    // Still inside the window: rejected outright, and the events list is
    // unchanged — a failed invocation publishes nothing.
    assert_eq!(
        s.client.try_finalize_dispute(&id),
        Err(Ok(Error::ChallengeActive))
    );
    let events = s.last_events();
    assert_eq!(
        events.len(),
        0,
        "a rejected finalization must publish no events"
    );
    let rejected = DisputeFinalizedEvent {
        channel_id: id,
        receiver_payout: 300,
        sender_refund: DEPOSIT - 300,
    }
    .to_xdr(&s.env, &s.contract);
    assert!(events.iter().all(|e| e != &rejected));

    // The channel is untouched: still Disputed, still awaiting the window.
    assert_eq!(s.client.get_channel(&id).phase, ChannelPhase::Disputed);
}
