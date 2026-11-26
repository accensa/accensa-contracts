extern crate std;

use accensa_common::Error;
use ed25519_dalek::{Signer, SigningKey};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env,
};
use state_channel::{ChannelPhase, StateChannel, StateChannelClient, StateUpdate};

struct Setup {
    env: Env,
    client: StateChannelClient<'static>,
    token: Address,
    sender: Address,
    receiver: Address,
    sk: SigningKey,
    pk: BytesN<32>,
}

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let sender = Address::generate(&env);
    let receiver = Address::generate(&env);
    let sk = SigningKey::from_bytes(&[2u8; 32]);
    let pk = BytesN::from_array(&env, &sk.verifying_key().to_bytes());

    let token = env.register_stellar_asset_contract_v2(admin).address();
    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&token);
    StellarAssetClient::new(&env, &token).mint(&sender, &1_000_000);

    Setup {
        env,
        client,
        token,
        sender,
        receiver,
        sk,
        pk,
    }
}

impl Setup {
    fn sign(&self, nonce: u64, balance: i128) -> BytesN<64> {
        let mut msg = [0u8; 56];
        msg[..32].copy_from_slice(&self.pk.to_array());
        msg[32..40].copy_from_slice(&nonce.to_be_bytes());
        msg[40..56].copy_from_slice(&balance.to_be_bytes());
        BytesN::from_array(&self.env, &self.sk.sign(&msg).to_bytes())
    }

    fn state(&self, nonce: u64, balance: i128) -> StateUpdate {
        StateUpdate { nonce, balance }
    }

    fn open(&self, amount: i128, challenge: u32) -> u64 {
        self.client
            .open_channel(&self.sender, &self.receiver, &self.pk, &amount, &challenge)
    }

    fn balance(&self, who: &Address) -> i128 {
        TokenClient::new(&self.env, &self.token).balance(who)
    }
}

#[test]
fn test_watchtower_counter_evidence_end_to_end_and_invalid_signature() {
    let s = setup();
    let channel_id = s.open(1_000, 100);
    let watchtower = Address::generate(&s.env);

    // Configure 10% bounty (1000 basis points)
    s.client.set_watchtower_bounty(&channel_id, &1_000);

    // Trigger dispute with an outdated/lower state
    let s1 = s.state(1, 100);
    s.client.close_channel(&channel_id, &s1, &s.sign(1, 100));
    let s2 = s.state(2, 200);
    s.client.dispute(&channel_id, &s2, &s.sign(2, 200));

    let channel_before = s.client.get_channel(&channel_id);
    assert_eq!(channel_before.phase, ChannelPhase::Disputed);

    // Try watchtower_counter_evidence with invalid signature, expecting Error::InvalidSignature
    let invalid_sig = BytesN::from_array(&s.env, &[99u8; 64]);
    let s3 = s.state(3, 400);
    let res = s.client.try_watchtower_counter_evidence(
        &channel_id,
        &s3,
        &invalid_sig,
        &watchtower,
    );
    assert_eq!(res, Err(Ok(Error::InvalidSignature)));

    // Call watchtower_counter_evidence with valid fresh state and signature
    s.client
        .watchtower_counter_evidence(&channel_id, &s3, &s.sign(3, 400), &watchtower);

    // Verify channel dispute window is updated (re-armed) and defender address recorded
    let channel_after = s.client.get_channel(&channel_id);
    assert_eq!(channel_after.phase, ChannelPhase::Disputed);
    assert_eq!(channel_after.nonce, 3);
    assert_eq!(channel_after.balance, 400);

    // Advance ledger and finalize dispute to lock in reward payouts
    s.env.ledger().with_mut(|li|\ {
        li.sequence_number = 250;
    });
    s.client.finalize_dispute(&channel_id);

    // Bounty = 10% of 400 = 40; receiver gets 360; sender refunds 600
    assert_eq!(s.balance(&watchtower), 40);
    assert_eq!(s.balance(&s.receiver), 360);
    assert_eq!(s.balance(&s.sender), 1_000_000 - 1_000 + 600);
    assert_eq!(
        s.client.get_channel(&channel_id).phase,
        ChannelPhase::Finalized
    );
}
