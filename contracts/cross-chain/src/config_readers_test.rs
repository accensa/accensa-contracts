//! Config-reader and `require_admin` guard tests (issue #653).
//!
//! Every assertion goes through the contract client — no test reads storage
//! directly. The two `require_admin` verdicts are proven *without mocking any
//! authorization*: [`GuardProbe`] is a tiny contract that drives the setters
//! with its own address as `admin`, and the host auto-authorizes a
//! `require_auth` for the **direct invoker** contract, so `require_auth`
//! succeeds while only `require_admin`'s comparison decides the typed outcome.

#![cfg(test)]

use accensa_common::Error;
use soroban_sdk::{contract, contractimpl, testutils::Address as _, Address, BytesN, Env};

use crate::{CrossChainBridge, CrossChainBridgeClient, PeerAddress};

/// Minimal caller used to reach both `require_admin` rejection arms with no
/// mocked auth. It passes its own address as the setter's `admin`: as the
/// direct invoker of the bridge call its `require_auth` is honored by the host
/// (the same mechanism `layerzero_test::MockEndpoint` relies on), so the
/// bridge's typed verdict — `NotInitialized` or `Unauthorized` — is what the
/// test observes.
#[contract]
struct GuardProbe;

#[contractimpl]
impl GuardProbe {
    pub fn set_gateway(env: Env, bridge: Address, gateway: Address) -> Result<(), Error> {
        match CrossChainBridgeClient::new(&env, &bridge)
            .try_set_axelar_gateway(&env.current_contract_address(), &gateway)
        {
            Ok(_) => Ok(()),
            // The bridge's own typed error propagates to the test's client.
            Err(Ok(e)) => Err(e),
            // An invocation-level failure would strip the typed verdict the
            // test asserts on; fail loudly instead.
            Err(Err(_)) => panic!("bridge invocation failed below the contract error level"),
        }
    }

    pub fn set_peer(env: Env, bridge: Address, peer: PeerAddress) -> Result<(), Error> {
        match CrossChainBridgeClient::new(&env, &bridge)
            .try_set_trusted_peer(&env.current_contract_address(), &peer)
        {
            Ok(_) => Ok(()),
            Err(Ok(e)) => Err(e),
            Err(Err(_)) => panic!("bridge invocation failed below the contract error level"),
        }
    }
}

fn peer(env: &Env, seed: u8) -> PeerAddress {
    PeerAddress {
        chain_id: 101,
        sender: BytesN::from_array(env, &[seed; 32]),
    }
}

// ── Uninitialized reads ──────────────────────────────────────────────────

#[test]
fn config_getters_are_unset_before_initialize() {
    let env = Env::default();
    let id = env.register(CrossChainBridge, ());
    let client = CrossChainBridgeClient::new(&env, &id);

    // Option-arm getter: plain None, never a panic.
    assert_eq!(client.get_axelar_gateway(), None);
    // Result-arm getters: typed NotInitialized, never a panic.
    assert_eq!(
        client.try_get_token(),
        Err(Ok(Error::NotInitialized)),
        "get_token must report NotInitialized before initialize"
    );
    assert_eq!(
        client.try_get_destination_chain_id(),
        Err(Ok(Error::NotInitialized)),
        "get_destination_chain_id must report NotInitialized before initialize"
    );
    // Uninitialized peer store simply has no bindings.
    let p = peer(&env, 9);
    assert!(!client.is_trusted_peer(&p.chain_id, &p.sender));
}

// ── Initialized reads and rebinds ────────────────────────────────────────

#[test]
fn config_getters_read_the_written_values_and_follow_rebinds() {
    let env = Env::default();
    env.mock_all_auths(); // happy paths only (rebinds below)
    let id = env.register(CrossChainBridge, ());
    let client = CrossChainBridgeClient::new(&env, &id);

    let admin = Address::generate(&env);
    let token = Address::generate(&env);
    let gateway_a = Address::generate(&env);
    let gateway_b = Address::generate(&env);
    client.initialize(&admin, &token, &7);

    // Exact values written by initialize.
    assert_eq!(client.get_token(), token);
    assert_eq!(
        client.get_destination_chain_id(),
        7,
        "the getter must return exactly the destination chain id written at initialize"
    );
    assert_eq!(client.get_axelar_gateway(), None);

    // Gateway: unset -> set -> rebound, the getter follows.
    client.set_axelar_gateway(&admin, &gateway_a);
    assert_eq!(client.get_axelar_gateway(), Some(gateway_a.clone()));
    client.set_axelar_gateway(&admin, &gateway_b);
    assert_eq!(
        client.get_axelar_gateway(),
        Some(gateway_b),
        "set_axelar_gateway must replace the stored gateway and the getter must follow"
    );

    // Trusted peer: unset -> set -> rebound, the getter follows.
    let p1 = peer(&env, 1);
    let p2 = peer(&env, 2);
    assert!(!client.is_trusted_peer(&p1.chain_id, &p1.sender));
    client.set_trusted_peer(&admin, &p1);
    assert!(client.is_trusted_peer(&p1.chain_id, &p1.sender));
    client.set_trusted_peer(&admin, &p2);
    assert_eq!(
        client.is_trusted_peer(&p2.chain_id, &p2.sender),
        true,
        "set_trusted_peer must register the rebound peer and is_trusted_peer must follow"
    );
    // Bindings are per (chain, sender): the earlier peer stays trusted.
    assert!(client.is_trusted_peer(&p1.chain_id, &p1.sender));
}

// ── Both require_admin verdicts, unmocked (issue #653) ───────────────────

#[test]
fn require_admin_reports_not_initialized_before_initialize_through_a_setter() {
    // Deliberately no env.mock_all_auths(): the probe's own require_auth is
    // satisfied by the host as the direct invoker, so the observed error is
    // require_admin's verdict, not a missing-authorization failure.
    let env = Env::default();
    let bridge_id = env.register(CrossChainBridge, ());
    let probe = GuardProbeClient::new(&env, &env.register(GuardProbe, ()));

    assert_eq!(
        probe.try_set_gateway(&bridge_id, &Address::generate(&env)),
        Err(Ok(Error::NotInitialized)),
        "a setter before initialize must surface require_admin's NotInitialized arm"
    );
}

#[test]
fn require_admin_reports_unauthorized_for_a_non_admin_caller_through_a_setter() {
    // Deliberately no env.mock_all_auths() — see above.
    let env = Env::default();
    let bridge_id = env.register(CrossChainBridge, ());
    let bridge = CrossChainBridgeClient::new(&env, &bridge_id);
    let probe = GuardProbeClient::new(&env, &env.register(GuardProbe, ()));

    // initialize performs no require_auth, so binding the real admin needs no
    // mocked auth either; the probe is then a caller that passes
    // require_auth (direct invoker) but is not that admin.
    let admin = Address::generate(&env);
    bridge.initialize(&admin, &Address::generate(&env), &1);

    assert_eq!(
        probe.try_set_peer(&bridge_id, &peer(&env, 3)),
        Err(Ok(Error::Unauthorized)),
        "a non-admin caller that satisfies require_auth must surface require_admin's Unauthorized arm"
    );
}
