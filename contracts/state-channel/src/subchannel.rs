//! Sub-channels (channels within channels) (issue #480).
//!
//! Enables participants to recursively open micro-channels bounded by the state
//! of a parent channel, allowing hierarchical payment routing:
//!
//! - [`open_subchannel`] binds a micro-channel to a parent channel and its current
//!   sequence nonce, carving out capacity from the parent's available escrow.
//! - [`update_subchannel`] advances the sub-channel state with monotonically increasing
//!   nonces up to the sub-channel's allocated capacity.
//! - [`settle_subchannel`] resolves the sub-channel before parent channel closure,
//!   rolling the committed sub-channel balance back into the parent channel's balance
//!   and releasing unused reservation.

use accensa_common::Error;
use soroban_sdk::{contractevent, contracttype, Address, Env};

use crate::{ChannelPhase, DataKey};

/// Sub-channel storage keys.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubChannelKey {
    /// Persistent storage for subchannel record.
    SubChannel(u64),
    /// Persistent counter for generating subchannel IDs.
    SubChannelCount,
    /// Number of unresolved subchannels currently active under a parent channel.
    ActiveCount(u64),
    /// Escrow reserved by active subchannels on a parent channel.
    ReservedEscrow(u64),
}

/// A micro-channel opened within a parent state channel.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubChannel {
    /// Unique identifier for this sub-channel.
    pub subchannel_id: u64,
    /// Pointer to the parent channel.
    pub parent_channel_id: u64,
    /// Sender authorized on the parent channel.
    pub sender: Address,
    /// Receiver entitled to sub-channel claims.
    pub receiver: Address,
    /// Total escrow capacity allocated from the parent channel.
    pub amount: i128,
    /// Parent channel sequence nonce at the time this sub-channel was opened.
    pub parent_sequence: u64,
    /// Latest sub-channel state sequence nonce.
    pub nonce: u64,
    /// Cumulative balance owed to the receiver under this sub-channel.
    pub balance: i128,
    /// Whether this sub-channel has been resolved/settled.
    pub settled: bool,
}

/// Emitted when a sub-channel is opened inside a parent channel.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubChannelOpenedEvent {
    #[topic]
    pub parent_channel_id: u64,
    #[topic]
    pub subchannel_id: u64,
    pub amount: i128,
    pub parent_sequence: u64,
}

/// Emitted when a sub-channel state is updated.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubChannelUpdatedEvent {
    #[topic]
    pub subchannel_id: u64,
    pub nonce: u64,
    pub balance: i128,
}

/// Emitted when a sub-channel is settled before parent channel closure.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubChannelSettledEvent {
    #[topic]
    pub parent_channel_id: u64,
    #[topic]
    pub subchannel_id: u64,
    pub final_balance: i128,
}

/// Total escrow currently reserved by active sub-channels on `parent_channel_id`.
pub fn reserved(env: &Env, parent_channel_id: u64) -> i128 {
    env.storage()
        .persistent()
        .get(&SubChannelKey::ReservedEscrow(parent_channel_id))
        .unwrap_or(0)
}

/// Number of active (unresolved) sub-channels under `parent_channel_id`.
pub fn active_subchannels(env: &Env, parent_channel_id: u64) -> u32 {
    env.storage()
        .persistent()
        .get(&SubChannelKey::ActiveCount(parent_channel_id))
        .unwrap_or(0)
}

/// Read a sub-channel by ID.
pub fn get_subchannel(env: &Env, subchannel_id: u64) -> Result<SubChannel, Error> {
    env.storage()
        .persistent()
        .get(&SubChannelKey::SubChannel(subchannel_id))
        .ok_or(Error::ChannelNotFound)
}

/// Opens a new sub-channel bounded by `parent_channel_id`.
pub fn open_subchannel(
    env: &Env,
    parent_channel_id: u64,
    receiver: Address,
    amount: i128,
) -> Result<u64, Error> {
    let parent = crate::StateChannel::get_channel(env.clone(), parent_channel_id)?;
    if parent.phase != ChannelPhase::Open {
        return Err(Error::ChannelNotOpen);
    }
    parent.sender.require_auth();

    if amount <= 0 {
        return Err(Error::InvalidAmount);
    }

    // Ensure parent has sufficient uncommitted escrow capacity
    let sub_reserved = reserved(env, parent_channel_id);
    let committed = parent
        .balance
        .checked_add(sub_reserved)
        .ok_or(Error::MathOverflow)?;

    if committed
        .checked_add(amount)
        .is_none_or(|total| total > parent.amount)
    {
        return Err(Error::HtlcInsufficientEscrow);
    }

    let subchannel_id: u64 = env
        .storage()
        .persistent()
        .get(&SubChannelKey::SubChannelCount)
        .unwrap_or(0)
        + 1;
    env.storage()
        .persistent()
        .set(&SubChannelKey::SubChannelCount, &subchannel_id);

    let subchannel = SubChannel {
        subchannel_id,
        parent_channel_id,
        sender: parent.sender.clone(),
        receiver: receiver.clone(),
        amount,
        parent_sequence: parent.nonce,
        nonce: 0,
        balance: 0,
        settled: false,
    };

    env.storage()
        .persistent()
        .set(&SubChannelKey::SubChannel(subchannel_id), &subchannel);

    let new_reserved = sub_reserved
        .checked_add(amount)
        .ok_or(Error::MathOverflow)?;
    env.storage().persistent().set(
        &SubChannelKey::ReservedEscrow(parent_channel_id),
        &new_reserved,
    );

    let active_count = active_subchannels(env, parent_channel_id) + 1;
    env.storage().persistent().set(
        &SubChannelKey::ActiveCount(parent_channel_id),
        &active_count,
    );

    SubChannelOpenedEvent {
        parent_channel_id,
        subchannel_id,
        amount,
        parent_sequence: parent.nonce,
    }
    .publish(env);

    Ok(subchannel_id)
}

/// Updates the off-chain state of a sub-channel with a new balance and nonce.
pub fn update_subchannel(
    env: &Env,
    subchannel_id: u64,
    new_nonce: u64,
    new_balance: i128,
) -> Result<(), Error> {
    let mut sub = get_subchannel(env, subchannel_id)?;
    if sub.settled {
        return Err(Error::ChannelAlreadyClosed);
    }

    let parent = crate::StateChannel::get_channel(env.clone(), sub.parent_channel_id)?;
    if parent.phase != ChannelPhase::Open {
        return Err(Error::ChannelNotOpen);
    }

    sub.sender.require_auth();

    if new_nonce <= sub.nonce {
        return Err(Error::StaleState);
    }
    if new_balance < 0 || new_balance > sub.amount {
        return Err(Error::ExceedsPayment);
    }

    sub.nonce = new_nonce;
    sub.balance = new_balance;

    env.storage()
        .persistent()
        .set(&SubChannelKey::SubChannel(subchannel_id), &sub);

    SubChannelUpdatedEvent {
        subchannel_id,
        nonce: new_nonce,
        balance: new_balance,
    }
    .publish(env);

    Ok(())
}

/// Settles a sub-channel into its parent channel before parent closure.
pub fn settle_subchannel(env: &Env, subchannel_id: u64) -> Result<(), Error> {
    let mut sub = get_subchannel(env, subchannel_id)?;
    if sub.settled {
        return Err(Error::ChannelAlreadyClosed);
    }

    let mut parent = crate::StateChannel::get_channel(env.clone(), sub.parent_channel_id)?;
    if parent.phase != ChannelPhase::Open {
        return Err(Error::ChannelNotOpen);
    }

    // Mark subchannel as settled
    sub.settled = true;
    env.storage()
        .persistent()
        .set(&SubChannelKey::SubChannel(subchannel_id), &sub);

    // Roll subchannel receiver's balance into parent channel balance
    parent.balance = parent
        .balance
        .checked_add(sub.balance)
        .ok_or(Error::MathOverflow)?;
    if parent.balance > parent.amount {
        return Err(Error::ExceedsPayment);
    }
    env.storage()
        .instance()
        .set(&DataKey::Channel(parent_channel_id(&sub)), &parent);

    // Release subchannel escrow reservation
    let sub_reserved = reserved(env, sub.parent_channel_id);
    let new_reserved = sub_reserved.saturating_sub(sub.amount);
    env.storage().persistent().set(
        &SubChannelKey::ReservedEscrow(sub.parent_channel_id),
        &new_reserved,
    );

    let active_count = active_subchannels(env, sub.parent_channel_id).saturating_sub(1);
    env.storage().persistent().set(
        &SubChannelKey::ActiveCount(sub.parent_channel_id),
        &active_count,
    );

    SubChannelSettledEvent {
        parent_channel_id: sub.parent_channel_id,
        subchannel_id,
        final_balance: sub.balance,
    }
    .publish(env);

    Ok(())
}

fn parent_channel_id(sub: &SubChannel) -> u64 {
    sub.parent_channel_id
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, token::StellarAssetClient, BytesN};

    struct TestHarness {
        env: Env,
        contract_id: Address,
        _sender: Address,
        _receiver: Address,
        channel_id: u64,
    }

    fn setup() -> TestHarness {
        let env = Env::default();
        env.mock_all_auths();

        let token_admin = Address::generate(&env);
        let sac = env.register_stellar_asset_contract_v2(token_admin);
        let token = sac.address();

        let sender = Address::generate(&env);
        let receiver = Address::generate(&env);
        StellarAssetClient::new(&env, &token).mint(&sender, &1_000_000);

        let contract_id = env.register(crate::StateChannel, ());
        let client = crate::StateChannelClient::new(&env, &contract_id);
        client.initialize(&token);

        let pubkey = BytesN::from_array(&env, &[1u8; 32]);
        let channel_id = client.open_channel(&sender, &receiver, &pubkey, &100_000, &100);

        TestHarness {
            env,
            contract_id,
            _sender: sender,
            _receiver: receiver,
            channel_id,
        }
    }

    #[test]
    fn test_open_update_settle_subchannel() {
        let h = setup();
        let sub_receiver = Address::generate(&h.env);

        let sub_id = h.env.as_contract(&h.contract_id, || {
            open_subchannel(&h.env, h.channel_id, sub_receiver.clone(), 30_000).unwrap()
        });
        assert_eq!(sub_id, 1);

        h.env.as_contract(&h.contract_id, || {
            assert_eq!(active_subchannels(&h.env, h.channel_id), 1);
            assert_eq!(reserved(&h.env, h.channel_id), 30_000);

            let sub = get_subchannel(&h.env, sub_id).unwrap();
            assert_eq!(sub.parent_channel_id, h.channel_id);
            assert_eq!(sub.parent_sequence, 0);
            assert_eq!(sub.amount, 30_000);
            assert_eq!(sub.balance, 0);
            assert!(!sub.settled);

            // Update sub-channel state
            update_subchannel(&h.env, sub_id, 1, 15_000).unwrap();
            let updated = get_subchannel(&h.env, sub_id).unwrap();
            assert_eq!(updated.nonce, 1);
            assert_eq!(updated.balance, 15_000);

            // Settle sub-channel
            settle_subchannel(&h.env, sub_id).unwrap();
            let settled = get_subchannel(&h.env, sub_id).unwrap();
            assert!(settled.settled);
            assert_eq!(active_subchannels(&h.env, h.channel_id), 0);
            assert_eq!(reserved(&h.env, h.channel_id), 0);

            let parent = crate::StateChannel::get_channel(h.env.clone(), h.channel_id).unwrap();
            assert_eq!(parent.balance, 15_000);
        });
    }

    #[test]
    fn test_subchannel_exceeding_parent_escrow_fails() {
        let h = setup();
        let sub_receiver = Address::generate(&h.env);

        h.env.as_contract(&h.contract_id, || {
            let err = open_subchannel(&h.env, h.channel_id, sub_receiver, 150_000);
            assert_eq!(err, Err(Error::HtlcInsufficientEscrow));
        });
    }

    #[test]
    fn test_subchannel_cannot_settle_twice() {
        let h = setup();
        let sub_receiver = Address::generate(&h.env);

        h.env.as_contract(&h.contract_id, || {
            let sub_id = open_subchannel(&h.env, h.channel_id, sub_receiver, 20_000).unwrap();
            settle_subchannel(&h.env, sub_id).unwrap();

            let err = settle_subchannel(&h.env, sub_id);
            assert_eq!(err, Err(Error::ChannelAlreadyClosed));
        });
    }
}
