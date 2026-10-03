//! LayerZero dispute-bridge integration tests (issue #455).
//!
//! A mock endpoint contract proves the real authorization path: when the
//! registered endpoint is the *direct caller* of `lz_receive`, the host
//! auto-authorizes the endpoint's `require_auth` on its own address — the
//! same mechanism `Governance::execute` relies on for governed calls. A
//! direct call from an outside address with no auth entries must therefore
//! fail.

#![cfg(test)]

extern crate std;

use accensa_common::Error;
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Events as _},
    Address, Bytes, BytesN, Env, Event,
};

use crate::{
    layerzero::{
        encode_dispute_payload, DisputeResolution, DisputeResolvedEvent, PeerAddress,
        DISPUTE_PAYLOAD_LEN,
    },
    CrossChainBridge, CrossChainBridgeClient,
};

/// Minimal stand-in for a LayerZero endpoint: forwards one delivery into the
/// bridge with its own contract address as the endpoint identity. Errors
/// from the bridge propagate as host traps, like a real endpoint would.
#[contract]
struct MockEndpoint;

#[contractimpl]
impl MockEndpoint {
    pub fn deliver(
        env: Env,
        bridge: Address,
        src_eid: u32,
        sender: BytesN<32>,
        nonce: u64,
        payload: Bytes,
    ) -> DisputeResolution {
        CrossChainBridgeClient::new(&env, &bridge).lz_receive(
            &env.current_contract_address(),
            &src_eid,
            &sender,
            &nonce,
            &payload,
        )
    }
}

struct Setup {
    env: Env,
    bridge: CrossChainBridgeClient<'static>,
    bridge_id: Address,
    admin: Address,
    endpoint_id: Address,
    endpoint: MockEndpointClient<'static>,
    peer: BytesN<32>,
    src_eid: u32,
}

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let bridge_id = env.register(CrossChainBridge, ());
    let bridge = CrossChainBridgeClient::new(&env, &bridge_id);
    bridge.initialize(&admin, &Address::generate(&env), &1);

    let endpoint_id = env.register(MockEndpoint, ());
    let endpoint = MockEndpointClient::new(&env, &endpoint_id);
    bridge.set_layerzero_endpoint(&admin, &endpoint_id);

    // Ethereum-style 20-byte peer address, left-padded into 32 bytes.
    let mut peer_bytes = [0u8; 32];
    peer_bytes[12..].copy_from_slice(&[0xab_u8; 20]);
    let peer = BytesN::from_array(&env, &peer_bytes);
    let src_eid = 101u32;
    bridge.set_trusted_peer(
        &admin,
        &PeerAddress {
            chain_id: src_eid,
            sender: peer.clone(),
        },
    );

    Setup {
        env,
        bridge,
        bridge_id,
        admin,
        endpoint_id,
        endpoint,
        peer,
        src_eid,
    }
}

impl Setup {
    fn payload(&self, dispute_id: &BytesN<32>, amount: u64) -> Bytes {
        encode_dispute_payload(
            &self.env,
            dispute_id,
            &BytesN::from_array(&self.env, &[9u8; 32]),
            amount,
            1_700_000_000,
        )
    }

    fn dispute_id(&self, seed: u8) -> BytesN<32> {
        BytesN::from_array(&self.env, &[seed; 32])
    }
}

#[test]
fn authorized_endpoint_delivers_dispute_resolution() {
    let s = setup();
    let dispute_id = s.dispute_id(1);
    let payload = s.payload(&dispute_id, 500);

    let resolution = s
        .endpoint
        .deliver(&s.bridge_id, &s.src_eid, &s.peer, &1, &payload);

    // `events().all()` covers the last invocation only, so capture the
    // delivery's events before any other contract call.
    let events = s.env.events().all().filter_by_contract(&s.bridge_id);

    assert_eq!(resolution.dispute_id, dispute_id);
    assert_eq!(resolution.approved_amount, 500);
    assert_eq!(resolution.src_chain_id, s.src_eid);
    assert_eq!(resolution.peer, s.peer);
    assert_eq!(resolution.settled_at, 1_700_000_000);

    // Delivery state is queryable and the event was published.
    assert!(s.bridge.is_dispute_settled(&dispute_id));
    assert_eq!(s.bridge.get_peer_nonce(&s.src_eid, &s.peer), 1);
    assert_eq!(
        events.events(),
        &[DisputeResolvedEvent {
            dispute_id: dispute_id.clone(),
            resolution_hash: BytesN::from_array(&s.env, &[9u8; 32]),
            approved_amount: 500,
            src_chain_id: s.src_eid,
            peer: s.peer.clone(),
            nonce: 1,
            settled_ledger: s.env.ledger().sequence(),
        }
        .to_xdr(&s.env, &s.bridge_id)]
    );
}

#[test]
fn later_nonces_are_accepted_and_advance_the_channel() {
    let s = setup();
    let first = s.dispute_id(1);
    s.endpoint.deliver(
        &s.bridge_id,
        &s.src_eid,
        &s.peer,
        &1,
        &s.payload(&first, 100),
    );
    let second = s.dispute_id(2);
    s.endpoint.deliver(
        &s.bridge_id,
        &s.src_eid,
        &s.peer,
        &7,
        &s.payload(&second, 200),
    );
    assert_eq!(s.bridge.get_peer_nonce(&s.src_eid, &s.peer), 7);
    assert!(s.bridge.is_dispute_settled(&second));
}

#[test]
fn untrusted_peer_is_rejected() {
    let s = setup();
    let mut other_bytes = [0u8; 32];
    other_bytes[12..].copy_from_slice(&[0xcd_u8; 20]);
    let stranger = BytesN::from_array(&s.env, &other_bytes);

    assert_eq!(
        s.bridge.try_lz_receive(
            &s.endpoint_id,
            &s.src_eid,
            &stranger,
            &1,
            &s.payload(&s.dispute_id(1), 100),
        ),
        Err(Ok(Error::Unauthorized))
    );
    // Nothing was recorded.
    assert!(!s.bridge.is_dispute_settled(&s.dispute_id(1)));
    assert_eq!(s.bridge.get_peer_nonce(&s.src_eid, &stranger), 0);
}

#[test]
fn wrong_chain_is_rejected_even_for_a_trusted_peer() {
    let s = setup();
    assert_eq!(
        s.bridge.try_lz_receive(
            &s.endpoint_id,
            // Trusted on 101, not on 102.
            &102,
            &s.peer,
            &1,
            &s.payload(&s.dispute_id(1), 100),
        ),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn replayed_and_stale_nonces_are_rejected() {
    let s = setup();
    let dispute_id = s.dispute_id(1);
    s.endpoint.deliver(
        &s.bridge_id,
        &s.src_eid,
        &s.peer,
        &5,
        &s.payload(&dispute_id, 100),
    );

    // Same nonce again: replay.
    assert_eq!(
        s.bridge.try_lz_receive(
            &s.endpoint_id,
            &s.src_eid,
            &s.peer,
            &5,
            &s.payload(&s.dispute_id(2), 100),
        ),
        Err(Ok(Error::StaleState))
    );
    // Older nonce: out of order.
    assert_eq!(
        s.bridge.try_lz_receive(
            &s.endpoint_id,
            &s.src_eid,
            &s.peer,
            &4,
            &s.payload(&s.dispute_id(3), 100),
        ),
        Err(Ok(Error::StaleState))
    );
}

#[test]
fn settled_dispute_cannot_be_settled_again() {
    let s = setup();
    let dispute_id = s.dispute_id(1);
    s.endpoint.deliver(
        &s.bridge_id,
        &s.src_eid,
        &s.peer,
        &1,
        &s.payload(&dispute_id, 100),
    );

    // A fresh packet nonce but the same dispute id must not settle twice.
    assert_eq!(
        s.bridge.try_lz_receive(
            &s.endpoint_id,
            &s.src_eid,
            &s.peer,
            &2,
            &s.payload(&dispute_id, 100),
        ),
        Err(Ok(Error::AlreadyRefunded))
    );
}

#[test]
fn malformed_payload_is_rejected() {
    let s = setup();
    let full = s.payload(&s.dispute_id(1), 100);
    let truncated = full.slice(0..(DISPUTE_PAYLOAD_LEN as u32 - 1));
    assert_eq!(
        s.bridge
            .try_lz_receive(&s.endpoint_id, &s.src_eid, &s.peer, &1, &truncated),
        Err(Ok(Error::InvalidProof))
    );

    // A future version byte is rejected even at the right length.
    let mut raw = std::vec![0u8; DISPUTE_PAYLOAD_LEN];
    full.copy_into_slice(&mut raw);
    raw[0] = 2;
    assert_eq!(
        s.bridge.try_lz_receive(
            &s.endpoint_id,
            &s.src_eid,
            &s.peer,
            &1,
            &Bytes::from_slice(&s.env, &raw),
        ),
        Err(Ok(Error::InvalidProof))
    );
}

#[test]
fn unconfigured_endpoint_fails_closed() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let bridge_id = env.register(CrossChainBridge, ());
    let bridge = CrossChainBridgeClient::new(&env, &bridge_id);
    bridge.initialize(&admin, &Address::generate(&env), &1);

    // No endpoint has been registered: any delivery must be refused.
    let endpoint_id = env.register(MockEndpoint, ());
    assert_eq!(
        bridge.try_lz_receive(
            &endpoint_id,
            &101u32,
            &BytesN::from_array(&env, &[0u8; 32]),
            &1,
            &encode_dispute_payload(
                &env,
                &BytesN::from_array(&env, &[1u8; 32]),
                &BytesN::from_array(&env, &[2u8; 32]),
                1,
                0,
            ),
        ),
        Err(Ok(Error::NotInitialized))
    );
}

#[test]
fn paused_bridge_rejects_delivery() {
    let s = setup();
    s.bridge.pause(&s.admin);
    assert_eq!(
        s.bridge.try_lz_receive(
            &s.endpoint_id,
            &s.src_eid,
            &s.peer,
            &1,
            &s.payload(&s.dispute_id(1), 100),
        ),
        Err(Ok(Error::Paused))
    );
}

#[test]
fn admin_gated_configuration_rejects_non_admin() {
    let s = setup();
    let attacker = Address::generate(&s.env);

    assert_eq!(
        s.bridge
            .try_set_layerzero_endpoint(&attacker, &Address::generate(&s.env),),
        Err(Ok(Error::Unauthorized))
    );
    assert_eq!(
        s.bridge.try_set_trusted_peer(
            &attacker,
            &PeerAddress {
                chain_id: 999,
                sender: BytesN::from_array(&s.env, &[1u8; 32]),
            },
        ),
        Err(Ok(Error::Unauthorized))
    );
}

#[test]
fn endpoint_rotation_requires_the_new_endpoint_for_delivery() {
    let s = setup();
    let new_endpoint_id = s.env.register(MockEndpoint, ());
    s.bridge.set_layerzero_endpoint(&s.admin, &new_endpoint_id);
    assert_eq!(s.bridge.get_layerzero_endpoint(), new_endpoint_id);

    // The old endpoint is no longer accepted...
    assert_eq!(
        s.bridge.try_lz_receive(
            &s.endpoint_id,
            &s.src_eid,
            &s.peer,
            &1,
            &s.payload(&s.dispute_id(1), 100),
        ),
        Err(Ok(Error::Unauthorized))
    );
    // ...while the rotated one delivers.
    let new_endpoint = MockEndpointClient::new(&s.env, &new_endpoint_id);
    new_endpoint.deliver(
        &s.bridge_id,
        &s.src_eid,
        &s.peer,
        &1,
        &s.payload(&s.dispute_id(1), 100),
    );
    assert!(s.bridge.is_dispute_settled(&s.dispute_id(1)));
}

/// Without auth entries, a direct call claiming the endpoint's identity
/// cannot satisfy `endpoint.require_auth()` — only the real endpoint
/// contract calling in can.
#[test]
#[should_panic]
fn direct_call_without_endpoint_auth_panics() {
    let s = setup();
    s.env.set_auths(&[]);
    let _ = CrossChainBridgeClient::new(&s.env, &s.bridge_id).lz_receive(
        &s.endpoint_id,
        &s.src_eid,
        &s.peer,
        &1,
        &s.payload(&s.dispute_id(1), 100),
    );
}
