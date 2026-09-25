//! Standardized event schema and granular emission helpers for Accensa contracts (issue #463).
//!
//! External GraphQL indexers (e.g. Subgraphs, SubQuery, Mercury, Horizon) require
//! a predictable, consistent topic structure: `[Protocol, Module, Action]`.
//!
//! - `Protocol`: Identifies the protocol, canonically `symbol_short!("accensa")`.
//! - `Module`: Identifies the contract domain, e.g. `vault`, `channel`, `receipt`,
//!   `bridge`, `gov`, `treasury`, etc.
//! - `Action`: Identifies the state transition, e.g. `deposit`, `withdraw`, `refund`,
//!   `transfer`, `open`, `close`, `dispute`, `anchor`, `prune`, etc.
//!
//! Event payloads carry granular metadata including `sender`, `receiver`, `asset`,
//! `amount`, and `fee`, allowing indexers to build comprehensive subgraphs and analytical
//! queries without secondary RPC lookups.

use soroban_sdk::{contracttype, symbol_short, Address, Bytes, BytesN, Env, IntoVal, Symbol, Val};

/// Canonical protocol symbol used as the first topic across all events.
pub const PROTOCOL: Symbol = symbol_short!("accensa");

/// Standard module symbols for Accensa contracts.
pub mod modules {
    use soroban_sdk::{symbol_short, Symbol};

    pub fn vault() -> Symbol {
        symbol_short!("vault")
    }
    pub fn channel() -> Symbol {
        symbol_short!("channel")
    }
    pub fn receipt() -> Symbol {
        symbol_short!("receipt")
    }
    pub fn bridge() -> Symbol {
        symbol_short!("bridge")
    }
    pub fn gov() -> Symbol {
        symbol_short!("gov")
    }
    pub fn treasury() -> Symbol {
        symbol_short!("treasury")
    }
    pub fn multisig() -> Symbol {
        symbol_short!("multisig")
    }
    pub fn auth() -> Symbol {
        symbol_short!("auth")
    }
}

/// Standard action symbols for contract mutations.
pub mod actions {
    use soroban_sdk::{symbol_short, Symbol};

    pub fn deposit() -> Symbol {
        symbol_short!("deposit")
    }
    pub fn withdraw() -> Symbol {
        symbol_short!("withdraw")
    }
    pub fn refund() -> Symbol {
        symbol_short!("refund")
    }
    pub fn transfer() -> Symbol {
        symbol_short!("transfer")
    }
    pub fn open() -> Symbol {
        symbol_short!("open")
    }
    pub fn close() -> Symbol {
        symbol_short!("close")
    }
    pub fn dispute() -> Symbol {
        symbol_short!("dispute")
    }
    pub fn claim() -> Symbol {
        symbol_short!("claim")
    }
    pub fn settle() -> Symbol {
        symbol_short!("settle")
    }
    pub fn anchor() -> Symbol {
        symbol_short!("anchor")
    }
    pub fn prune() -> Symbol {
        symbol_short!("prune")
    }
    pub fn pause() -> Symbol {
        symbol_short!("pause")
    }
    pub fn unpause() -> Symbol {
        symbol_short!("unpause")
    }
}

/// Universal granular event payload for indexers.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GranularEventPayload {
    pub sender: Option<Address>,
    pub receiver: Option<Address>,
    pub asset: Option<Address>,
    pub amount: i128,
    pub fee: i128,
    pub timestamp: u64,
    pub sequence: u64,
    pub metadata: Option<Bytes>,
}

/// Granular transfer or payout event payload.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferEventPayload {
    pub sender: Address,
    pub receiver: Address,
    pub asset: Address,
    pub amount: i128,
    pub fee: i128,
    pub timestamp: u64,
}

/// Granular refund event payload.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefundEventPayload {
    pub merchant: Address,
    pub recipient: Address,
    pub asset: Address,
    pub amount: i128,
    pub fee: i128,
    pub payment_ref: BytesN<32>,
    pub timestamp: u64,
}

/// Granular channel mutation event payload.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelStatePayload {
    pub channel_id: u64,
    pub sender: Address,
    pub receiver: Address,
    pub asset: Option<Address>,
    pub balance: i128,
    pub nonce: u64,
    pub fee: i128,
    pub timestamp: u64,
}

/// Granular batch/anchor mutation event payload.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnchorEventPayload {
    pub shard_id: u64,
    pub batch_id: u64,
    pub root: BytesN<32>,
    pub count: u32,
    pub timestamp: u64,
}

/// Emit an event with standardized `[Protocol, Module, Action]` topics.
#[allow(deprecated)]
pub fn emit_event<T: IntoVal<Env, Val>>(env: &Env, module: Symbol, action: Symbol, payload: T) {
    env.events().publish((PROTOCOL, module, action), payload);
}

/// Emit a granular event payload under `[accensa, module, action]`.
pub fn emit_granular(env: &Env, module: Symbol, action: Symbol, payload: &GranularEventPayload) {
    emit_event(env, module, action, payload.clone());
}

/// Emit a granular transfer event under `[accensa, module, transfer]`.
pub fn emit_transfer(
    env: &Env,
    module: Symbol,
    sender: Address,
    receiver: Address,
    asset: Address,
    amount: i128,
    fee: i128,
) {
    let payload = TransferEventPayload {
        sender,
        receiver,
        asset,
        amount,
        fee,
        timestamp: env.ledger().timestamp(),
    };
    emit_event(env, module, actions::transfer(), payload);
}

/// Emit a granular refund event under `[accensa, module, refund]`.
pub fn emit_refund(
    env: &Env,
    module: Symbol,
    merchant: Address,
    recipient: Address,
    asset: Address,
    amount: i128,
    fee: i128,
    payment_ref: BytesN<32>,
) {
    let payload = RefundEventPayload {
        merchant,
        recipient,
        asset,
        amount,
        fee,
        payment_ref,
        timestamp: env.ledger().timestamp(),
    };
    emit_event(env, module, actions::refund(), payload);
}

/// Emit a granular channel state update event under `[accensa, channel, settle]`.
pub fn emit_channel_state(
    env: &Env,
    channel_id: u64,
    sender: Address,
    receiver: Address,
    asset: Option<Address>,
    balance: i128,
    nonce: u64,
    fee: i128,
) {
    let payload = ChannelStatePayload {
        channel_id,
        sender,
        receiver,
        asset,
        balance,
        nonce,
        fee,
        timestamp: env.ledger().timestamp(),
    };
    emit_event(env, modules::channel(), actions::settle(), payload);
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        contract, contractimpl,
        testutils::{Address as _, Events, Ledger},
        vec, Address, BytesN, Env, IntoVal, Symbol,
    };

    #[contract]
    pub struct TestEmitterContract;

    #[contractimpl]
    impl TestEmitterContract {
        pub fn emit_g(env: Env, module: Symbol, action: Symbol, payload: GranularEventPayload) {
            emit_granular(&env, module, action, &payload);
        }

        pub fn emit_t(
            env: Env,
            module: Symbol,
            sender: Address,
            receiver: Address,
            asset: Address,
            amount: i128,
            fee: i128,
        ) {
            emit_transfer(&env, module, sender, receiver, asset, amount, fee);
        }

        pub fn emit_r(
            env: Env,
            module: Symbol,
            merchant: Address,
            recipient: Address,
            asset: Address,
            amount: i128,
            fee: i128,
            p_ref: BytesN<32>,
        ) {
            emit_refund(&env, module, merchant, recipient, asset, amount, fee, p_ref);
        }

        pub fn emit_c(
            env: Env,
            channel_id: u64,
            sender: Address,
            receiver: Address,
            asset: Option<Address>,
            balance: i128,
            nonce: u64,
            fee: i128,
        ) {
            emit_channel_state(
                &env, channel_id, sender, receiver, asset, balance, nonce, fee,
            );
        }
    }

    #[test]
    fn test_standardized_event_topics_and_payload() {
        let env = Env::default();
        let contract_id = env.register(TestEmitterContract, ());
        let client = TestEmitterContractClient::new(&env, &contract_id);
        env.ledger().set_timestamp(1700000000);

        let sender = Address::generate(&env);
        let receiver = Address::generate(&env);
        let asset = Address::generate(&env);

        let payload = GranularEventPayload {
            sender: Some(sender.clone()),
            receiver: Some(receiver.clone()),
            asset: Some(asset.clone()),
            amount: 50_000,
            fee: 250,
            timestamp: 1700000000,
            sequence: 1,
            metadata: None,
        };

        client.emit_g(&modules::vault(), &actions::deposit(), &payload);

        let events = env.events().all().filter_by_contract(&contract_id);
        assert_eq!(events.events().len(), 1);

        assert_eq!(
            events,
            vec![
                &env,
                (
                    contract_id.clone(),
                    (PROTOCOL, modules::vault(), actions::deposit()).into_val(&env),
                    payload.into_val(&env)
                )
            ]
        );
    }

    #[test]
    fn test_granular_transfer_and_refund_events() {
        let env = Env::default();
        let contract_id = env.register(TestEmitterContract, ());
        let client = TestEmitterContractClient::new(&env, &contract_id);
        env.ledger().set_timestamp(1700000050);

        let sender = Address::generate(&env);
        let receiver = Address::generate(&env);
        let asset = Address::generate(&env);

        client.emit_t(&modules::treasury(), &sender, &receiver, &asset, &1000, &10);
        let events = env.events().all().filter_by_contract(&contract_id);
        assert_eq!(events.events().len(), 1);

        let expected_transfer = TransferEventPayload {
            sender: sender.clone(),
            receiver: receiver.clone(),
            asset: asset.clone(),
            amount: 1000,
            fee: 10,
            timestamp: 1700000050,
        };

        assert_eq!(
            events,
            vec![
                &env,
                (
                    contract_id.clone(),
                    (PROTOCOL, modules::treasury(), actions::transfer()).into_val(&env),
                    expected_transfer.into_val(&env)
                )
            ]
        );

        let p_ref = BytesN::from_array(&env, &[7u8; 32]);
        client.emit_r(
            &modules::vault(),
            &sender,
            &receiver,
            &asset,
            &500,
            &5,
            &p_ref,
        );
        let events2 = env.events().all().filter_by_contract(&contract_id);
        assert_eq!(events2.events().len(), 1);

        let expected_refund = RefundEventPayload {
            merchant: sender,
            recipient: receiver,
            asset,
            amount: 500,
            fee: 5,
            payment_ref: p_ref,
            timestamp: 1700000050,
        };

        assert_eq!(
            events2,
            vec![
                &env,
                (
                    contract_id,
                    (PROTOCOL, modules::vault(), actions::refund()).into_val(&env),
                    expected_refund.into_val(&env)
                )
            ]
        );
    }

    #[test]
    fn test_granular_channel_state_event() {
        let env = Env::default();
        let contract_id = env.register(TestEmitterContract, ());
        let client = TestEmitterContractClient::new(&env, &contract_id);
        env.ledger().set_timestamp(1700000100);

        let sender = Address::generate(&env);
        let receiver = Address::generate(&env);
        let asset = Address::generate(&env);

        client.emit_c(
            &42,
            &sender,
            &receiver,
            &Some(asset.clone()),
            &9999,
            &12,
            &1,
        );

        let events = env.events().all().filter_by_contract(&contract_id);
        assert_eq!(events.events().len(), 1);

        let expected_channel = ChannelStatePayload {
            channel_id: 42,
            sender,
            receiver,
            asset: Some(asset),
            balance: 9999,
            nonce: 12,
            fee: 1,
            timestamp: 1700000100,
        };

        assert_eq!(
            events,
            vec![
                &env,
                (
                    contract_id,
                    (PROTOCOL, modules::channel(), actions::settle()).into_val(&env),
                    expected_channel.into_val(&env)
                )
            ]
        );
    }
}
