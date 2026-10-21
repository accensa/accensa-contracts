//! Multi-asset collateral pooling tests (issue #423).

extern crate std;

use super::*;
use crate::multi_asset::{
    state_payload, BalanceRecord, MultiAssetChannelOpenedEvent, MultiAssetSettledEvent,
    MultiAssetState, MultiAssetStateUpdatedEvent,
};
use ed25519_dalek::{Signer, SigningKey};
use soroban_sdk::{
    map,
    testutils::{Address as _, Events as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, BytesN, Env, Event, Map,
};

const DEPOSIT_USDC: i128 = 1_000;
const DEPOSIT_EURC: i128 = 500;
const DEPOSIT_XLM: i128 = 20_000;
const CHALLENGE: u32 = 100;

struct Setup {
    env: Env,
    client: StateChannelClient<'static>,
    contract: Address,
    sender: Address,
    receiver: Address,
    usdc: Address,
    eurc: Address,
    xlm: Address,
    sk: SigningKey,
}

fn new_token(env: &Env, holder: &Address) -> Address {
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(env))
        .address();
    StellarAssetClient::new(env, &token).mint(holder, &1_000_000);
    token
}

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let sender = Address::generate(&env);
    let receiver = Address::generate(&env);
    let usdc = new_token(&env, &sender);
    let eurc = new_token(&env, &sender);
    let xlm = new_token(&env, &sender);

    let contract = env.register(StateChannel, ());
    let client = StateChannelClient::new(&env, &contract);
    client.initialize(&usdc);

    Setup {
        env,
        client,
        contract,
        sender,
        receiver,
        usdc,
        eurc,
        xlm,
        sk: SigningKey::from_bytes(&[3u8; 32]),
    }
}

impl Setup {
    fn pubkey(&self) -> BytesN<32> {
        BytesN::from_array(&self.env, &self.sk.verifying_key().to_bytes())
    }

    fn deposits(&self) -> Map<Address, i128> {
        map![
            &self.env,
            (self.usdc.clone(), DEPOSIT_USDC),
            (self.eurc.clone(), DEPOSIT_EURC),
            (self.xlm.clone(), DEPOSIT_XLM)
        ]
    }

    fn open(&self) -> u64 {
        self.client.open_multi_asset_channel(
            &self.sender,
            &self.receiver,
            &self.pubkey(),
            &self.deposits(),
            &CHALLENGE,
        )
    }

    fn state(&self, nonce: u64, usdc: i128, eurc: i128, xlm: i128) -> MultiAssetState {
        MultiAssetState {
            nonce,
            balances: map![
                &self.env,
                (self.usdc.clone(), usdc),
                (self.eurc.clone(), eurc),
                (self.xlm.clone(), xlm)
            ],
        }
    }

    fn sign(&self, channel_id: u64, state: &MultiAssetState) -> BytesN<64> {
        let payload = state_payload(&self.env, &self.contract, channel_id, state);
        let mut msg = std::vec![0u8; payload.len() as usize];
        payload.copy_into_slice(&mut msg);
        BytesN::from_array(&self.env, &self.sk.sign(&msg).to_bytes())
    }

    fn balance(&self, token: &Address, who: &Address) -> i128 {
        TokenClient::new(&self.env, token).balance(who)
    }

    fn advance(&self, ledgers: u32) {
        let seq = self.env.ledger().sequence();
        self.env.ledger().set_sequence_number(seq + ledgers);
    }
}

#[test]
fn open_escrows_every_asset() {
    let s = setup();
    let id = s.open();

    let channel = s.client.get_multi_asset_channel(&id);
    assert_eq!(channel.assets.len(), 3);
    assert_eq!(
        channel.assets.get(s.eurc.clone()).unwrap(),
        BalanceRecord {
            deposit: DEPOSIT_EURC,
            receiver_balance: 0
        }
    );
    assert_eq!(s.balance(&s.usdc, &s.contract), DEPOSIT_USDC);
    assert_eq!(s.balance(&s.eurc, &s.contract), DEPOSIT_EURC);
    assert_eq!(s.balance(&s.xlm, &s.contract), DEPOSIT_XLM);
}

#[test]
fn single_close_settles_all_assets_with_per_asset_conservation() {
    let s = setup();
    let id = s.open();

    let st1 = s.state(1, 100, 50, 0);
    s.client
        .update_multi_asset_state(&id, &st1, &s.sign(id, &st1));
    let st2 = s.state(2, 400, 500, 7_000);
    s.client
        .close_multi_asset_channel(&id, &st2, &s.sign(id, &st2));

    s.advance(CHALLENGE + 1);
    let sender_before = [
        s.balance(&s.usdc, &s.sender),
        s.balance(&s.eurc, &s.sender),
        s.balance(&s.xlm, &s.sender),
    ];
    s.client.settle_multi_asset_channel(&id);

    let expected = [
        (&s.usdc, 400, DEPOSIT_USDC),
        (&s.eurc, 500, DEPOSIT_EURC),
        (&s.xlm, 7_000, DEPOSIT_XLM),
    ];
    for (i, (token, paid, deposit)) in expected.into_iter().enumerate() {
        assert_eq!(s.balance(token, &s.receiver), paid);
        assert_eq!(
            s.balance(token, &s.sender) - sender_before[i],
            deposit - paid
        );
        assert_eq!(s.balance(token, &s.contract), 0);
    }
    assert_eq!(
        s.client.get_multi_asset_channel(&id).phase,
        ChannelPhase::Finalized
    );
    assert_eq!(
        s.client.try_settle_multi_asset_channel(&id),
        Err(Ok(Error::ChannelAlreadyClosed))
    );
}

#[test]
fn substituted_currency_is_rejected() {
    let s = setup();
    let id = s.open();
    let other = new_token(&s.env, &s.sender);

    // Same number of assets, but EURC swapped for a token the channel
    // never escrowed.
    let st = MultiAssetState {
        nonce: 1,
        balances: map![&s.env, (s.usdc.clone(), 1), (other, 1), (s.xlm.clone(), 1)],
    };
    assert_eq!(
        s.client
            .try_update_multi_asset_state(&id, &st, &s.sign(id, &st)),
        Err(Ok(Error::UnsupportedAsset))
    );
}

#[test]
fn omitted_asset_is_rejected() {
    let s = setup();
    let id = s.open();
    let st = MultiAssetState {
        nonce: 1,
        balances: map![&s.env, (s.usdc.clone(), 1), (s.xlm.clone(), 1)],
    };
    assert_eq!(
        s.client
            .try_close_multi_asset_channel(&id, &st, &s.sign(id, &st)),
        Err(Ok(Error::UnsupportedAsset))
    );
}

#[test]
fn value_cannot_move_between_assets() {
    let s = setup();
    let id = s.open();
    // Total value would fit in the pool, but EURC exceeds its own deposit.
    let st = s.state(1, 0, DEPOSIT_EURC + 1, 0);
    assert_eq!(
        s.client
            .try_update_multi_asset_state(&id, &st, &s.sign(id, &st)),
        Err(Ok(Error::ExceedsPayment))
    );
    let negative = s.state(1, -1, 0, 0);
    assert_eq!(
        s.client
            .try_update_multi_asset_state(&id, &negative, &s.sign(id, &negative)),
        Err(Ok(Error::ExceedsPayment))
    );
}

#[test]
fn stale_state_is_rejected() {
    let s = setup();
    let id = s.open();
    let st = s.state(5, 1, 1, 1);
    s.client
        .update_multi_asset_state(&id, &st, &s.sign(id, &st));

    let old = s.state(5, 2, 2, 2);
    assert_eq!(
        s.client
            .try_update_multi_asset_state(&id, &old, &s.sign(id, &old)),
        Err(Ok(Error::StaleState))
    );
    let older = s.state(4, 2, 2, 2);
    assert_eq!(
        s.client
            .try_close_multi_asset_channel(&id, &older, &s.sign(id, &older)),
        Err(Ok(Error::StaleState))
    );
}

#[test]
fn newer_state_during_challenge_window_wins() {
    let s = setup();
    let id = s.open();

    // Sender closes with an old, favourable state.
    let old = s.state(1, 10, 10, 10);
    s.client
        .close_multi_asset_channel(&id, &old, &s.sign(id, &old));
    assert_eq!(
        s.client.try_settle_multi_asset_channel(&id),
        Err(Ok(Error::ChallengeActive))
    );

    // Receiver answers with the latest state inside the window.
    s.advance(CHALLENGE / 2);
    let newer = s.state(2, 900, 400, 15_000);
    s.client
        .update_multi_asset_state(&id, &newer, &s.sign(id, &newer));

    // The challenge re-armed the window.
    s.advance(CHALLENGE / 2 + 1);
    assert_eq!(
        s.client.try_settle_multi_asset_channel(&id),
        Err(Ok(Error::ChallengeActive))
    );
    s.advance(CHALLENGE);
    s.client.settle_multi_asset_channel(&id);

    assert_eq!(s.balance(&s.usdc, &s.receiver), 900);
    assert_eq!(s.balance(&s.eurc, &s.receiver), 400);
    assert_eq!(s.balance(&s.xlm, &s.receiver), 15_000);
}

#[test]
fn update_after_challenge_window_is_rejected() {
    let s = setup();
    let id = s.open();
    let st = s.state(1, 1, 1, 1);
    s.client
        .close_multi_asset_channel(&id, &st, &s.sign(id, &st));
    s.advance(CHALLENGE + 1);

    let late = s.state(2, 2, 2, 2);
    assert_eq!(
        s.client
            .try_update_multi_asset_state(&id, &late, &s.sign(id, &late)),
        Err(Ok(Error::ChallengeExpired))
    );
}

#[test]
#[should_panic]
fn state_signed_for_another_channel_is_rejected() {
    let s = setup();
    let a = s.open();
    let b = s.open();
    let st = s.state(1, 1, 1, 1);
    s.client.update_multi_asset_state(&b, &st, &s.sign(a, &st));
}

#[test]
fn open_channel_settles_only_after_expiry() {
    let s = setup();
    let id = s.open();
    let st = s.state(1, 250, 0, 1);
    s.client
        .update_multi_asset_state(&id, &st, &s.sign(id, &st));

    assert_eq!(
        s.client.try_settle_multi_asset_channel(&id),
        Err(Ok(Error::ChallengeActive))
    );
    s.advance(s.client.get_max_channel_lifetime() + 1);
    s.client.settle_multi_asset_channel(&id);

    assert_eq!(s.balance(&s.usdc, &s.receiver), 250);
    assert_eq!(s.balance(&s.eurc, &s.receiver), 0);
    assert_eq!(s.balance(&s.xlm, &s.receiver), 1);
    assert_eq!(s.balance(&s.eurc, &s.contract), 0);
}

#[test]
fn invalid_deposits_are_rejected() {
    let s = setup();
    let pk = s.pubkey();
    assert_eq!(
        s.client.try_open_multi_asset_channel(
            &s.sender,
            &s.receiver,
            &pk,
            &Map::new(&s.env),
            &CHALLENGE
        ),
        Err(Ok(Error::InvalidAmount))
    );
    assert_eq!(
        s.client.try_open_multi_asset_channel(
            &s.sender,
            &s.receiver,
            &pk,
            &map![&s.env, (s.usdc.clone(), 10), (s.eurc.clone(), 0)],
            &CHALLENGE
        ),
        Err(Ok(Error::InvalidAmount))
    );
}

#[test]
fn ids_are_shared_with_single_asset_channels() {
    let s = setup();
    let single = s
        .client
        .open_channel(&s.sender, &s.receiver, &s.pubkey(), &10, &CHALLENGE);
    let multi = s.open();
    assert_eq!(multi, single + 1);
    assert_eq!(
        s.client.try_get_multi_asset_channel(&single),
        Err(Ok(Error::ChannelNotFound))
    );
}

// ── Event shapes and storage-key isolation (issue #611) ─────────────────────

/// Assert the last invocation published exactly `expected` for `contract`, in
/// publish order, by XDR comparison — the same style as `hashlock_test.rs`.
/// `env.events().all()` covers only the last invocation, so this must run
/// immediately after the call under test and before any other contract call
/// (including token balance reads).
fn assert_emitted(
    env: &Env,
    contract: &Address,
    expected: std::vec::Vec<soroban_sdk::xdr::ContractEvent>,
) {
    assert_eq!(env.events().all().filter_by_contract(contract), expected);
}

#[test]
fn open_publishes_the_opened_event_with_every_deposit() {
    let s = setup();
    let id = s.open();

    assert_emitted(
        &s.env,
        &s.contract,
        std::vec![MultiAssetChannelOpenedEvent {
            channel_id: id,
            sender: s.sender.clone(),
            receiver: s.receiver.clone(),
            deposits: s.deposits(),
            challenge_period: CHALLENGE,
        }
        .to_xdr(&s.env, &s.contract)],
    );
}

#[test]
fn update_publishes_the_state_updated_event_with_the_open_phase() {
    let s = setup();
    let id = s.open();

    let st = s.state(1, 100, 50, 0);
    s.client
        .update_multi_asset_state(&id, &st, &s.sign(id, &st));

    assert_emitted(
        &s.env,
        &s.contract,
        std::vec![MultiAssetStateUpdatedEvent {
            channel_id: id,
            nonce: st.nonce,
            balances: st.balances.clone(),
            phase: ChannelPhase::Open,
        }
        .to_xdr(&s.env, &s.contract)],
    );
}

#[test]
fn close_publishes_the_state_updated_event_with_the_closed_phase() {
    let s = setup();
    let id = s.open();

    let st = s.state(1, 400, 500, 7_000);
    s.client
        .close_multi_asset_channel(&id, &st, &s.sign(id, &st));

    // The second `MultiAssetStateUpdatedEvent` publish site: same shape,
    // published by `close` with the channel already `Closed`.
    assert_emitted(
        &s.env,
        &s.contract,
        std::vec![MultiAssetStateUpdatedEvent {
            channel_id: id,
            nonce: st.nonce,
            balances: st.balances.clone(),
            phase: ChannelPhase::Closed,
        }
        .to_xdr(&s.env, &s.contract)],
    );
}

#[test]
fn settle_publishes_the_settled_event_and_the_balances_match_it() {
    let s = setup();
    let id = s.open();

    let st1 = s.state(1, 100, 50, 0);
    s.client
        .update_multi_asset_state(&id, &st1, &s.sign(id, &st1));
    let st2 = s.state(2, 400, 500, 7_000);
    s.client
        .close_multi_asset_channel(&id, &st2, &s.sign(id, &st2));
    s.advance(CHALLENGE + 1);

    let sender_before = [
        s.balance(&s.usdc, &s.sender),
        s.balance(&s.eurc, &s.sender),
        s.balance(&s.xlm, &s.sender),
    ];

    s.client.settle_multi_asset_channel(&id);
    assert_emitted(
        &s.env,
        &s.contract,
        std::vec![MultiAssetSettledEvent {
            channel_id: id,
            receiver_payouts: map![
                &s.env,
                (s.usdc.clone(), 400),
                (s.eurc.clone(), 500),
                (s.xlm.clone(), 7_000)
            ],
            sender_refunds: map![
                &s.env,
                (s.usdc.clone(), DEPOSIT_USDC - 400),
                (s.eurc.clone(), DEPOSIT_EURC - 500),
                (s.xlm.clone(), DEPOSIT_XLM - 7_000)
            ],
        }
        .to_xdr(&s.env, &s.contract)],
    );

    // The event's per-asset amounts are exactly what the token clients read
    // back: the receiver is paid `receiver_payouts`, the sender gets back
    // `sender_refunds`, and the escrow ends at zero in every asset.
    assert_eq!(s.balance(&s.usdc, &s.receiver), 400);
    assert_eq!(s.balance(&s.eurc, &s.receiver), 500);
    assert_eq!(s.balance(&s.xlm, &s.receiver), 7_000);
    assert_eq!(
        s.balance(&s.usdc, &s.sender) - sender_before[0],
        DEPOSIT_USDC - 400
    );
    assert_eq!(
        s.balance(&s.eurc, &s.sender) - sender_before[1],
        DEPOSIT_EURC - 500
    );
    assert_eq!(
        s.balance(&s.xlm, &s.sender) - sender_before[2],
        DEPOSIT_XLM - 7_000
    );
    assert_eq!(s.balance(&s.usdc, &s.contract), 0);
    assert_eq!(s.balance(&s.eurc, &s.contract), 0);
    assert_eq!(s.balance(&s.xlm, &s.contract), 0);
}

#[test]
fn single_asset_and_multi_asset_channels_use_distinct_storage_keys() {
    let s = setup();

    // Same counterparty and amount: only the `DataKey` variant differs.
    let single = s.client.open_channel(
        &s.sender,
        &s.receiver,
        &s.pubkey(),
        &DEPOSIT_USDC,
        &CHALLENGE,
    );
    let multi = s.open();
    assert_eq!(multi, single + 1);

    // Read the keys directly (they are crate-private, so this has to be a
    // `src/` test): `DataKey::MultiAssetChannel` must not resolve the record
    // written under `DataKey::Channel`, and vice versa.
    let single_via_multi_key: Option<MultiAssetChannel> = s.env.as_contract(&s.contract, || {
        s.env
            .storage()
            .persistent()
            .get(&DataKey::MultiAssetChannel(single))
    });
    assert!(single_via_multi_key.is_none());
    let multi_via_single_key: Option<Channel> = s.env.as_contract(&s.contract, || {
        s.env.storage().instance().get(&DataKey::Channel(multi))
    });
    assert!(multi_via_single_key.is_none());

    // Each variant resolves only its own record.
    let single_record: Option<Channel> = s.env.as_contract(&s.contract, || {
        s.env.storage().instance().get(&DataKey::Channel(single))
    });
    let single_record =
        single_record.expect("the single-asset record lives under DataKey::Channel");
    assert_eq!(single_record.amount, DEPOSIT_USDC);
    assert_eq!(single_record.sender, s.sender);
    assert_eq!(single_record.receiver, s.receiver);

    let multi_record: Option<MultiAssetChannel> = s.env.as_contract(&s.contract, || {
        s.env
            .storage()
            .persistent()
            .get(&DataKey::MultiAssetChannel(multi))
    });
    let multi_record =
        multi_record.expect("the multi-asset record lives under DataKey::MultiAssetChannel");
    assert_eq!(multi_record.sender, s.sender);
    assert_eq!(multi_record.receiver, s.receiver);
    assert_eq!(multi_record.assets.len(), 3);

    // The public read paths agree: neither id resolves in the other's
    // namespace, so a multi-asset channel id can never return the
    // single-asset record — the two key spaces stay distinct.
    assert_eq!(
        s.client.try_get_multi_asset_channel(&single),
        Err(Ok(Error::ChannelNotFound))
    );
    assert_eq!(
        s.client.try_get_channel(&multi),
        Err(Ok(Error::ChannelNotFound))
    );
    assert_eq!(s.client.get_channel(&single).amount, DEPOSIT_USDC);
    assert_eq!(s.client.get_multi_asset_channel(&multi).assets.len(), 3);
}
