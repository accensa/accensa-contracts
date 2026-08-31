#![no_std]

#[cfg(test)]
mod test;

use accensa_common::Error;
use soroban_sdk::{
    contract, contractevent, contractimpl, contractmeta, contracttype, Address, Bytes, BytesN, Env,
};

contractmeta!(key = "name", val = "StateChannel");
contractmeta!(key = "version", val = env!("CARGO_PKG_VERSION"));
contractmeta!(
    key = "repo",
    val = "https://github.com/accensa/accensa-contracts"
);

/// Channel state machine phases.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelPhase {
    /// Channel is open, states can be submitted.
    Open,
    /// Sender has closed the channel; dispute window is active.
    Closed,
    /// Channel has been finalized (either after dispute window or by claim).
    Finalized,
}

/// Persistent record for an open or recently closed channel.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Channel {
    /// The off-chain sender (merchant) who signs state updates.
    pub sender: Address,
    /// The on-chain receiver (agent) who can claim or dispute.
    pub receiver: Address,
    /// Escrowed token amount locked in the channel.
    pub amount: i128,
    /// Latest submitted state nonce (monotonically increasing).
    pub nonce: u64,
    /// Cumulative amount the receiver is entitled to, as of the latest state.
    pub balance: i128,
    /// The channel's lifecycle phase.
    pub phase: ChannelPhase,
    /// Ledger at which the channel was opened; used for timeout checks.
    pub opened_at: u32,
    /// Ledger at which the channel was closed; `0` if still open.
    pub closed_at: u32,
    /// Number of ledgers the dispute window remains open after `close_channel`.
    pub challenge_period: u32,
    /// Ed25519 public key used to verify off-chain state signatures.
    pub sender_pubkey: BytesN<32>,
}

/// A signed state update submitted by anyone.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateUpdate {
    /// Monotonically increasing nonce; prevents replay.
    pub nonce: u64,
    /// Cumulative amount the receiver is entitled to at this nonce.
    pub balance: i128,
}

/// Data keys for contract storage.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    Channel(u64),
    ChannelCount,
    Token,
    /// Maximum number of ledgers a channel can stay open before it expires.
    MaxChannelLifetime,
}

/// Emitted when a channel is opened.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelOpenedEvent {
    #[topic]
    pub channel_id: u64,
    pub sender: Address,
    pub receiver: Address,
    pub amount: i128,
    pub challenge_period: u32,
}

/// Emitted when a signed state update is submitted on-chain.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateUpdatedEvent {
    #[topic]
    pub channel_id: u64,
    pub nonce: u64,
    pub balance: i128,
}

/// Emitted when the sender cooperatively closes the channel.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelClosedEvent {
    #[topic]
    pub channel_id: u64,
    pub balance: i128,
    pub closed_at: u32,
}

/// Emitted when a receiver disputes a close with a newer state.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeEvent {
    #[topic]
    pub channel_id: u64,
    pub nonce: u64,
    pub balance: i128,
}

/// Emitted when the receiver claims funds after the dispute window expires.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimEvent {
    #[topic]
    pub channel_id: u64,
    pub amount: i128,
    pub recipient: Address,
}

/// Default dispute challenge period: ~1 hour at ~5 s/ledger = 720 ledgers.
const DEFAULT_CHALLENGE_PERIOD: u32 = 720;

/// Maximum challenge period: ~24 hours = 17,280 ledgers.
const MAX_CHALLENGE_PERIOD: u32 = 17_280;

/// Default channel lifetime: ~7 days = 1,209,600 ledgers.
const DEFAULT_MAX_CHANNEL_LIFETIME: u32 = 1_209_600;

/// TTL for channel storage entries (~30 days).
const TTL_EXTEND: u32 = 518_400;
const TTL_THRESHOLD: u32 = 100;

#[contract]
pub struct StateChannel;

#[contractimpl]
impl StateChannel {
    /// Initialize the state channel factory with a settlement token.
    pub fn initialize(env: Env, token: Address) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Token) {
            return Err(Error::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage().instance().set(&DataKey::ChannelCount, &0u64);
        env.storage()
            .instance()
            .set(&DataKey::MaxChannelLifetime, &DEFAULT_MAX_CHANNEL_LIFETIME);
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);
        Ok(())
    }

    /// Open a new unidirectional state channel.
    ///
    /// `sender` locks `amount` tokens in escrow. `sender_pubkey` is the
    /// Ed25519 public key used to verify off-chain state signatures. The
    /// channel expires after `MaxChannelLifetime` ledgers if not closed.
    pub fn open_channel(
        env: Env,
        sender: Address,
        receiver: Address,
        sender_pubkey: BytesN<32>,
        amount: i128,
        challenge_period: u32,
    ) -> Result<u64, Error> {
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        sender.require_auth();

        let token: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(Error::NotInitialized)?;

        let contract_addr = env.current_contract_address();
        soroban_sdk::token::Client::new(&env, &token).transfer(&sender, &contract_addr, &amount);

        let channel_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::ChannelCount)
            .unwrap_or(0)
            + 1;

        let effective_challenge = if challenge_period == 0 {
            DEFAULT_CHALLENGE_PERIOD
        } else {
            challenge_period.min(MAX_CHALLENGE_PERIOD)
        };

        let channel = Channel {
            sender: sender.clone(),
            receiver: receiver.clone(),
            amount,
            nonce: 0,
            balance: 0,
            phase: ChannelPhase::Open,
            opened_at: env.ledger().sequence(),
            closed_at: 0,
            challenge_period: effective_challenge,
            sender_pubkey,
        };

        env.storage()
            .instance()
            .set(&DataKey::Channel(channel_id), &channel);
        env.storage()
            .instance()
            .set(&DataKey::ChannelCount, &channel_id);
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND);

        ChannelOpenedEvent {
            channel_id,
            sender,
            receiver,
            amount,
            challenge_period: effective_challenge,
        }
        .publish(&env);

        Ok(channel_id)
    }

    /// Submit a signed state update for an open channel.
    pub fn update_state(
        env: Env,
        channel_id: u64,
        state: StateUpdate,
        signature: BytesN<64>,
    ) -> Result<(), Error> {
        let mut channel = Self::get_channel_internal(&env, channel_id)?;

        if channel.phase != ChannelPhase::Open {
            return Err(Error::ChannelNotOpen);
        }

        Self::verify_state_signature(&env, &channel, &state, &signature)?;

        if state.nonce <= channel.nonce {
            return Err(Error::StaleState);
        }

        if state.balance < 0 || state.balance > channel.amount {
            return Err(Error::ExceedsPayment);
        }

        channel.nonce = state.nonce;
        channel.balance = state.balance;

        env.storage()
            .instance()
            .set(&DataKey::Channel(channel_id), &channel);

        StateUpdatedEvent {
            channel_id,
            nonce: state.nonce,
            balance: state.balance,
        }
        .publish(&env);

        Ok(())
    }

    /// Cooperatively close the channel with the latest agreed state.
    pub fn close_channel(
        env: Env,
        channel_id: u64,
        state: StateUpdate,
        signature: BytesN<64>,
    ) -> Result<(), Error> {
        let mut channel = Self::get_channel_internal(&env, channel_id)?;

        if channel.phase != ChannelPhase::Open {
            return Err(Error::ChannelNotOpen);
        }

        let max_lifetime: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MaxChannelLifetime)
            .unwrap_or(DEFAULT_MAX_CHANNEL_LIFETIME);
        if env.ledger().sequence() > channel.opened_at + max_lifetime {
            return Err(Error::ChannelExpired);
        }

        Self::verify_state_signature(&env, &channel, &state, &signature)?;

        if state.balance < 0 || state.balance > channel.amount {
            return Err(Error::ExceedsPayment);
        }

        channel.nonce = state.nonce;
        channel.balance = state.balance;
        channel.phase = ChannelPhase::Closed;
        channel.closed_at = env.ledger().sequence();

        env.storage()
            .instance()
            .set(&DataKey::Channel(channel_id), &channel);

        ChannelClosedEvent {
            channel_id,
            balance: state.balance,
            closed_at: channel.closed_at,
        }
        .publish(&env);

        Ok(())
    }

    /// Dispute a cooperative close by submitting a newer signed state.
    pub fn dispute(
        env: Env,
        channel_id: u64,
        state: StateUpdate,
        signature: BytesN<64>,
    ) -> Result<(), Error> {
        let mut channel = Self::get_channel_internal(&env, channel_id)?;

        if channel.phase != ChannelPhase::Closed {
            return Err(Error::ChannelNotOpen);
        }

        let current_ledger = env.ledger().sequence();
        if current_ledger > channel.closed_at + channel.challenge_period {
            return Err(Error::ChallengeExpired);
        }

        Self::verify_state_signature(&env, &channel, &state, &signature)?;

        if state.nonce <= channel.nonce {
            return Err(Error::StaleState);
        }

        if state.balance < 0 || state.balance > channel.amount {
            return Err(Error::ExceedsPayment);
        }

        channel.nonce = state.nonce;
        channel.balance = state.balance;
        channel.phase = ChannelPhase::Open;
        channel.closed_at = 0;

        env.storage()
            .instance()
            .set(&DataKey::Channel(channel_id), &channel);

        DisputeEvent {
            channel_id,
            nonce: state.nonce,
            balance: state.balance,
        }
        .publish(&env);

        Ok(())
    }

    /// Claim funds after the dispute window has expired.
    pub fn claim(env: Env, channel_id: u64) -> Result<(), Error> {
        let mut channel = Self::get_channel_internal(&env, channel_id)?;

        if channel.phase != ChannelPhase::Closed {
            return Err(Error::ChallengeActive);
        }

        let current_ledger = env.ledger().sequence();
        if current_ledger <= channel.closed_at + channel.challenge_period {
            return Err(Error::ChallengeActive);
        }

        let token: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(Error::NotInitialized)?;

        let payout = channel.balance;

        if payout > 0 {
            let contract_addr = env.current_contract_address();
            soroban_sdk::token::Client::new(&env, &token).transfer(
                &contract_addr,
                &channel.receiver,
                &payout,
            );
        }

        channel.phase = ChannelPhase::Finalized;

        env.storage()
            .instance()
            .set(&DataKey::Channel(channel_id), &channel);

        ClaimEvent {
            channel_id,
            amount: payout,
            recipient: channel.receiver,
        }
        .publish(&env);

        Ok(())
    }

    /// Reclaim escrowed funds for an expired channel.
    pub fn reclaim(env: Env, channel_id: u64) -> Result<(), Error> {
        let mut channel = Self::get_channel_internal(&env, channel_id)?;

        if channel.phase != ChannelPhase::Open {
            return Err(Error::ChannelAlreadyClosed);
        }

        let max_lifetime: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MaxChannelLifetime)
            .unwrap_or(DEFAULT_MAX_CHANNEL_LIFETIME);
        let current_ledger = env.ledger().sequence();
        if current_ledger <= channel.opened_at + max_lifetime {
            return Err(Error::ChannelNotOpen);
        }

        let token: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(Error::NotInitialized)?;

        let refund = channel.amount - channel.balance;

        channel.phase = ChannelPhase::Finalized;

        env.storage()
            .instance()
            .set(&DataKey::Channel(channel_id), &channel);

        if refund > 0 {
            let contract_addr = env.current_contract_address();
            soroban_sdk::token::Client::new(&env, &token).transfer(
                &contract_addr,
                &channel.sender,
                &refund,
            );
        }

        Ok(())
    }

    /// Read a channel record.
    pub fn get_channel(env: Env, channel_id: u64) -> Result<Channel, Error> {
        Self::get_channel_internal(&env, channel_id)
    }

    /// Returns the total number of channels opened.
    pub fn get_channel_count(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::ChannelCount)
            .unwrap_or(0)
    }

    /// Returns the settlement token address.
    pub fn get_token(env: Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(Error::NotInitialized)
    }

    /// Returns the default challenge period.
    pub fn get_default_challenge_period(_env: Env) -> u32 {
        DEFAULT_CHALLENGE_PERIOD
    }

    /// Returns the maximum allowed challenge period.
    pub fn get_max_challenge_period(_env: Env) -> u32 {
        MAX_CHALLENGE_PERIOD
    }

    /// Returns the maximum channel lifetime.
    pub fn get_max_channel_lifetime(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::MaxChannelLifetime)
            .unwrap_or(DEFAULT_MAX_CHANNEL_LIFETIME)
    }

    // ── Internal helpers ─────────────────────────────────────────────────

    fn get_channel_internal(env: &Env, channel_id: u64) -> Result<Channel, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Channel(channel_id))
            .ok_or(Error::ChannelNotFound)
    }

    /// Verify that `signature` is a valid Ed25519 signature of the state's
    /// canonical representation by the channel's sender.
    ///
    /// On Soroban, `ed25519_verify` traps (transaction fails) if the
    /// signature is invalid — there is no boolean return. An invalid
    /// signature therefore aborts the transaction before any state changes
    /// are committed, which is the correct rejection behaviour for a state
    /// channel: the stale or forged state is never recorded.
    fn verify_state_signature(
        env: &Env,
        channel: &Channel,
        state: &StateUpdate,
        signature: &BytesN<64>,
    ) -> Result<(), Error> {
        let payload = Self::state_payload(env, channel, state);
        env.crypto()
            .ed25519_verify(&channel.sender_pubkey, &payload, signature);
        Ok(())
    }

    /// Build the canonical byte representation of a state update for signing.
    fn state_payload(env: &Env, channel: &Channel, state: &StateUpdate) -> Bytes {
        let mut buf = Bytes::new(env);
        // Sender public key (32 bytes)
        buf.extend_from_slice(&channel.sender_pubkey.to_array());
        // Nonce (8 bytes, big-endian)
        buf.extend_from_slice(&state.nonce.to_be_bytes());
        // Balance (16 bytes, big-endian i128)
        buf.extend_from_slice(&state.balance.to_be_bytes());
        buf
    }
}
