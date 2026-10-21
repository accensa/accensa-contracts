#![cfg(all(test, feature = "budget-assert"))]

//! Tier A budget assertions for `StateChannel`, using Tollcraft's
//! `soroban-budget-assert` `#[budget_cpu_lt(N)]` macro.
//!
//! Each attribute is a local WASM-mode CPU-estimate gate: the test fails if the
//! invocation's measured CPU instruction count exceeds `N`. `N` is the committed
//! failure threshold = `measured_baseline * 1.15`.
//!
//! The contract must be compiled to WASM first:
//! `cargo build -p state-channel --target wasm32v1-none --release`

extern crate std;
use std::format;
use std::string::{String, ToString};

use super::*;
use budget_macros::budget_cpu_lt;
use ed25519_dalek::{Signer, SigningKey};
use soroban_sdk::{
    testutils::Address as _, token::StellarAssetClient, Address, Bytes, BytesN, Env,
};

const DEPOSIT: i128 = 1_000;
const CHALLENGE: u32 = 100;

fn load_wasm(env: &Env, path: &str) -> Bytes {
    let buf = std::fs::read(path).expect(
        "state-channel wasm not found; run `cargo build -p state-channel \
         --target wasm32v1-none --release` before the budget tests",
    );
    Bytes::from_slice(env, &buf)
}

struct Setup<'a> {
    env: &'a Env,
    client: StateChannelClient<'a>,
    contract: Address,
    sender: Address,
    receiver: Address,
    sender_sk: SigningKey,
    receiver_sk: SigningKey,
}

fn setup(env: &Env) -> Setup<'_> {
    env.mock_all_auths();
    env.cost_estimate().disable_resource_limits();

    let sender = Address::generate(env);
    let receiver = Address::generate(env);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(env))
        .address();
    StellarAssetClient::new(env, &token).mint(&sender, &DEPOSIT);

    let wasm = load_wasm(env, "../../target/wasm32v1-none/release/state_channel.wasm");
    #[allow(deprecated)]
    let contract = env.register_contract_wasm(None, wasm);
    let client = StateChannelClient::new(env, &contract);
    client.initialize(&token);

    Setup {
        env,
        client,
        contract,
        sender,
        receiver,
        sender_sk: SigningKey::from_bytes(&[5u8; 32]),
        receiver_sk: SigningKey::from_bytes(&[6u8; 32]),
    }
}

fn pubkey(env: &Env, sk: &SigningKey) -> BytesN<32> {
    BytesN::from_array(env, &sk.verifying_key().to_bytes())
}

impl<'a> Setup<'a> {
    fn open(&self) -> u64 {
        let id = self.client.open_channel(
            &self.sender,
            &self.receiver,
            &pubkey(self.env, &self.sender_sk),
            &DEPOSIT,
            &CHALLENGE,
        );
        self.client
            .register_receiver_key(&id, &pubkey(self.env, &self.receiver_sk));
        id
    }

    fn sign_mutual(&self, state: &MutualCloseState) -> (BytesN<64>, BytesN<64>) {
        let payload = crate::close::close_payload(self.env, &self.contract, state);
        let mut msg = std::vec![0u8; payload.len() as usize];
        payload.copy_into_slice(&mut msg);
        let s_sig = BytesN::from_array(self.env, &self.sender_sk.sign(&msg).to_bytes());
        let r_sig = BytesN::from_array(self.env, &self.receiver_sk.sign(&msg).to_bytes());
        (s_sig, r_sig)
    }

    fn sign_update(&self, state: &StateUpdate) -> BytesN<64> {
        let pk = pubkey(self.env, &self.sender_sk).to_array();
        let mut msg = [0u8; 56];
        msg[..32].copy_from_slice(&pk);
        msg[32..40].copy_from_slice(&state.nonce.to_be_bytes());
        msg[40..56].copy_from_slice(&state.balance.to_be_bytes());
        BytesN::from_array(self.env, &self.sender_sk.sign(&msg).to_bytes())
    }
}

// Baseline: 2_074_544 CPU instructions measured on 2026-10-16.
// Ceiling = baseline * 1.15 = 2_385_726 (rounded to 2_390_000).
#[test]
#[budget_cpu_lt(2_390_000)]
fn budget_mutual_close() {
    let env = Env::default();
    let s = setup(&env);
    let id = s.open();
    let state = MutualCloseState {
        channel_id: id,
        receiver_balance: 700,
        sender_balance: 300,
    };
    let (s_sig, r_sig) = s.sign_mutual(&state);

    env.cost_estimate().budget().reset_unlimited();
    s.client.mutual_close(&state, &s_sig, &r_sig);
}

// Baseline: 1_362_123 CPU instructions measured on 2026-10-16.
// Ceiling = baseline * 1.15 = 1_566_442 (rounded to 1_570_000).
#[test]
#[budget_cpu_lt(1_570_000)]
fn budget_close_channel() {
    let env = Env::default();
    let s = setup(&env);
    let id = s.open();
    let state = StateUpdate {
        nonce: 1,
        balance: 500,
    };
    let sig = s.sign_update(&state);

    env.cost_estimate().budget().reset_unlimited();
    s.client.close_channel(&id, &state, &sig);
}
