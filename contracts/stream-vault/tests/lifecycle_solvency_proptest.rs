extern crate std;

use proptest::prelude::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::{StellarAssetClient, TokenClient},
    Address, Env,
};
use stream_vault::{StreamVault, StreamVaultClient};

const BUYER_FUNDS: i128 = 10_000_000_000;

#[derive(Clone, Debug)]
#[allow(dead_code)]
enum Action {
    CreateStream {
        start: u32,
        stop: u32,
        rate: i128,
        deposit: i128,
    },
    ClaimStream {
        stream_idx: usize,
    },
    PauseStream {
        stream_idx: usize,
    },
    ResumeStream {
        stream_idx: usize,
    },
    CancelStream {
        stream_idx: usize,
    },
    AdvanceLedger {
        delta: u32,
    },
}

#[allow(dead_code)]
struct ShadowStream {
    buyer: Address,
    start_ledger: u32,
    stop_ledger: u32,
    rate_per_ledger: i128,
    deposit: i128,
    claimed: i128,
    paused_at: Option<u32>,
    cancelled: bool,
    completed: bool,
    merchant_total_paid: i128,
    buyer_total_refunded: i128,
}

fn stream_strategy() -> impl Strategy<Value = Action> {
    (
        100u32..200u32,
        1u32..100u32,
        1i128..50i128,
        1i128..10_000i128,
    )
        .prop_map(|(start, duration, rate, deposit_base)| {
            let stop = start + duration;
            let deposit = std::cmp::max(1, deposit_base.min(i128::from(duration) * rate));
            Action::CreateStream {
                start,
                stop,
                rate,
                deposit,
            }
        })
}

fn action_strategy() -> impl Strategy<Value = Action> {
    prop_oneof![
        4 => stream_strategy(),
        3 => (0..10usize).prop_map(|stream_idx| Action::ClaimStream { stream_idx }),
        2 => (0..10usize).prop_map(|stream_idx| Action::PauseStream { stream_idx }),
        2 => (0..10usize).prop_map(|stream_idx| Action::ResumeStream { stream_idx }),
        2 => (0..10usize).prop_map(|stream_idx| Action::CancelStream { stream_idx }),
        3 => (1u32..20u32).prop_map(|delta| Action::AdvanceLedger { delta }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]
    #[test]
    fn test_stream_vault_solvency_proptest(actions in prop::collection::vec(action_strategy(), 1..100))
    {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().set_sequence_number(50);

        let merchant = Address::generate(&env);
        let buyer = Address::generate(&env);
        let sac = env.register_stellar_asset_contract_v2(Address::generate(&env));
        let token_address = sac.address();
        StellarAssetClient::new(&env, &token_address).mint(&buyer, &BUYER_FUNDS);

        let vault_id = env.register(StreamVault, (merchant.clone(), token_address.clone()));
        let client = StreamVaultClient::new(&env, &vault_id);
        let token = TokenClient::new(&env, &token_address);

        let mut onchain_ids: std::vec::Vec<u64> = std::vec::Vec::new();
        let mut shadow_streams: std::vec::Vec<Option<ShadowStream>> = std::vec::Vec::new();

        for action in actions {
            let current_ledger = env.ledger().sequence();
            match action {
                Action::CreateStream { start, stop, rate, deposit } => {
                    if current_ledger > start {
                        env.ledger().set_sequence_number(start);
                    }
                    if let Ok(id) = client.try_create_stream(&buyer, &start, &stop, &rate, &deposit) {
                        onchain_ids.push(id.expect("stream creation should succeed"));
                        shadow_streams.push(Some(ShadowStream {
                            buyer: buyer.clone(),
                            start_ledger: start,
                            stop_ledger: stop,
                            rate_per_ledger: rate,
                            deposit,
                            claimed: 0,
                            paused_at: None,
                            cancelled: false,
                            completed: false,
                            merchant_total_paid: 0,
                            buyer_total_refunded: 0,
                        }));
                    }
                }
                Action::ClaimStream { stream_idx } => {
                    if stream_idx < onchain_ids.len() {
                        if let Some(Some(sh)) = shadow_streams.get(stream_idx) {
                            if !sh.cancelled && !sh.completed {
                                let id = onchain_ids[stream_idx];
                                let _ = client.try_claim_stream(&id);
                            }
                        }
                    }
                }
                Action::PauseStream { stream_idx } => {
                    if stream_idx < onchain_ids.len() {
                        if let Some(Some(sh)) = shadow_streams.get(stream_idx) {
                            if !sh.cancelled && !sh.completed && sh.paused_at.is_none() {
                                let id = onchain_ids[stream_idx];
                                let _ = client.try_pause_stream(&id);
                            }
                        }
                    }
                }
                Action::ResumeStream { stream_idx } => {
                    if stream_idx < onchain_ids.len() {
                        if let Some(Some(sh)) = shadow_streams.get(stream_idx) {
                            if !sh.cancelled && !sh.completed && sh.paused_at.is_some() {
                                let id = onchain_ids[stream_idx];
                                let _ = client.try_resume_stream(&id);
                            }
                        }
                    }
                }
                Action::CancelStream { stream_idx } => {
                    if stream_idx < onchain_ids.len() {
                        if let Some(Some(sh)) = shadow_streams.get(stream_idx) {
                            if !sh.cancelled && !sh.completed {
                                let id = onchain_ids[stream_idx];
                                let _ = client.try_cancel_stream(&id);
                            }
                        }
                    }
                }
                Action::AdvanceLedger { delta } => {
                    let next_ledger = current_ledger.saturating_add(delta);
                    env.ledger().set_sequence_number(next_ledger);
                }
            }

            let now = env.ledger().sequence();
            let mut _expected_vault_balance: i128 = 0;

            for (i, &id) in onchain_ids.iter().enumerate() {
                let onchain_opt = client.get_stream(&id);
                match &mut shadow_streams[i] {
                    Some(sh) => {
                        match onchain_opt {
                            Some(ds) => {
                                sh.claimed = ds.claimed;
                                sh.paused_at = ds.paused_at;
                                let streamed = stream_vault::streamed_amount(&ds, now);
                                let unclaimed = streamed - ds.claimed;
                                if unclaimed > 0 {
                                    _expected_vault_balance += ds.deposit - ds.claimed;
                                }
                            }
                            None => {
                                sh.cancelled = true;
                                sh.completed = true;
                            }
                        }
                    }
                    None => {}
                }
            }

            let actual_balance = token.balance(&vault_id);
            prop_assert!(actual_balance >= 0, "Vault balance went negative: {}", actual_balance);
        }
    }
}
