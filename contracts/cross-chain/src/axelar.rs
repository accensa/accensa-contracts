//! Axelar Gateway cross-chain deposits (issue #454).
//!
//! Lets users on other chains (Ethereum, Polygon, ...) fund Soroban escrow
//! balances through the Axelar gateway. As with the other adapters in this
//! crate, the Soroban side does not verify Axelar's validator set itself - that
//! is the gateway's job. It only:
//!
//! - accepts `axelar_execute` from a **registered gateway** (admin-configured),
//! - asks that gateway to confirm the inbound message via
//!   [`AxelarGatewayInterface::validate_message`] before acting,
//! - bounds-checks the payload before decoding it
//!   ([`Error::InvalidProof`] on any structural violation), and
//! - records each executed `(message_id)` exactly once
//!   ([`Error::AlreadyRefunded`] on replay).
//!
//! # Deposit payload (`DEPOSIT_PAYLOAD_VERSION = 1`)
//!
//! ```text
//! [0]      version     (must be 1)
//! [1..33]  recipient   (32-byte bridged account id)
//! [33..49] amount      (BE i128, base units of the bridged asset; must be > 0)
//! ```
//!
//! 49 bytes total; any other length is rejected so a future version cannot
//! silently decode as v1.

use accensa_common::Error;
use soroban_sdk::{
    contractclient, contractevent, contracttype, Bytes, BytesN, Env, String,
};

/// Current deposit payload layout version accepted by [`parse_deposit_payload`].
pub const DEPOSIT_PAYLOAD_VERSION: u8 = 1;

/// Exact byte length of a version-1 deposit payload.
pub const DEPOSIT_PAYLOAD_LEN: usize = 1 + 32 + 16;

/// TTL low-water mark shared with the rest of the contract's storage policy.
pub(crate) const AXELAR_TTL_THRESHOLD: u32 = 100;

/// TTL extension target (~30 days at ~5 s/ledger).
pub(crate) const AXELAR_TTL_EXTEND: u32 = 518_400;

/// Minimal Axelar Soroban gateway interface used to validate an inbound message.
///
/// The gateway confirms that `(source_chain, message_id, source_address,
/// payload_hash)` was approved on the source chain. This crate never trusts the
/// caller's word for it.
#[contractclient(name = "AxelarGatewayClient")]
pub trait AxelarGatewayInterface {
    /// Return `true` when the gateway has approved the given message.
    fn validate_message(
        env: Env,
        source_chain: String,
        message_id: String,
        source_address: String,
        payload_hash: BytesN<32>,
    ) -> bool;
}

/// A validated cross-chain deposit.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AxelarDeposit {
    /// Bridged recipient account id (32 bytes) credited with the deposit.
    pub recipient: BytesN<32>,
    /// Amount of the bridged asset, in the asset's base units.
    pub amount: i128,
}

/// Emitted when an Axelar deposit is credited.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AxelarDepositCreditedEvent {
    #[topic]
    pub message_id: String,
    pub recipient: BytesN<32>,
    pub amount: i128,
    pub source_chain: String,
    /// Running total credited to `recipient` after this deposit.
    pub credited_total: i128,
    /// Ledger sequence at which the deposit was credited.
    pub credited_ledger: u32,
}

/// Storage keys owned by the Axelar half, kept separate from `crate::DataKey`
/// so existing serialized enum values are untouched.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    /// Persistent: total bridged asset credited to a recipient id.
    Credited(BytesN<32>),
    /// Persistent: Axelar message ids already executed (replay guard).
    MessageExecuted(String),
}

/// Encode a version-1 deposit payload (the wire format the source chain sends).
pub fn encode_deposit_payload(env: &Env, recipient: &BytesN<32>, amount: i128) -> Bytes {
    let mut buf = Bytes::new(env);
    buf.extend_from_slice(&[DEPOSIT_PAYLOAD_VERSION]);
    buf.extend_from_slice(&recipient.to_array());
    buf.extend_from_slice(&amount.to_be_bytes());
    buf
}

/// Parse and validate a version-1 deposit payload.
pub fn parse_deposit_payload(env: &Env, payload: &Bytes) -> Result<AxelarDeposit, Error> {
    let len = payload.len() as usize;
    if len != DEPOSIT_PAYLOAD_LEN {
        return Err(Error::InvalidProof);
    }

    let mut raw = [0u8; DEPOSIT_PAYLOAD_LEN];
    payload.copy_into_slice(&mut raw);

    if raw[0] != DEPOSIT_PAYLOAD_VERSION {
        return Err(Error::InvalidProof);
    }

    let mut recipient_bytes = [0u8; 32];
    recipient_bytes.copy_from_slice(&raw[1..33]);
    let recipient = BytesN::from_array(env, &recipient_bytes);

    let mut amount_bytes = [0u8; 16];
    amount_bytes.copy_from_slice(&raw[33..49]);
    let amount = i128::from_be_bytes(amount_bytes);
    if amount <= 0 {
        return Err(Error::InvalidAmount);
    }

    Ok(AxelarDeposit { recipient, amount })
}

/// Credit the bridged asset to `recipient`, guarding against message replay.
///
/// Returns the running total credited to the recipient.
pub(crate) fn record_deposit(
    env: &Env,
    deposit: AxelarDeposit,
    source_chain: String,
    message_id: String,
) -> Result<i128, Error> {
    // An executed message is final: the id can never be credited twice.
    let executed_key = DataKey::MessageExecuted(message_id.clone());
    if env.storage().persistent().has(&executed_key) {
        return Err(Error::AlreadyRefunded);
    }
    env.storage().persistent().set(&executed_key, &true);
    env.storage()
        .persistent()
        .extend_ttl(&executed_key, AXELAR_TTL_THRESHOLD, AXELAR_TTL_EXTEND);

    let credited_key = DataKey::Credited(deposit.recipient.clone());
    let current: i128 = env
        .storage()
        .persistent()
        .get(&credited_key)
        .unwrap_or(0);
    let new_total = current
        .checked_add(deposit.amount)
        .ok_or(Error::MathOverflow)?;
    env.storage().persistent().set(&credited_key, &new_total);
    env.storage()
        .persistent()
        .extend_ttl(&credited_key, AXELAR_TTL_THRESHOLD, AXELAR_TTL_EXTEND);

    AxelarDepositCreditedEvent {
        message_id,
        recipient: deposit.recipient,
        amount: deposit.amount,
        source_chain,
        credited_total: new_total,
        credited_ledger: env.ledger().sequence(),
    }
    .publish(env);

    Ok(new_total)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn encode_round_trips_through_parse() {
        let env = Env::default();
        let recipient = BytesN::from_array(&env, &[7u8; 32]);
        let payload = encode_deposit_payload(&env, &recipient, 1_000);
        assert_eq!(payload.len(), DEPOSIT_PAYLOAD_LEN as u32);

        let parsed = parse_deposit_payload(&env, &payload).unwrap();
        assert_eq!(parsed.recipient, recipient);
        assert_eq!(parsed.amount, 1_000);
    }

    #[test]
    fn wrong_length_is_rejected() {
        let env = Env::default();
        let payload = encode_deposit_payload(&env, &BytesN::from_array(&env, &[1u8; 32]), 5);
        let truncated = payload.slice(0..(DEPOSIT_PAYLOAD_LEN as u32 - 1));
        assert_eq!(
            parse_deposit_payload(&env, &truncated),
            Err(Error::InvalidProof)
        );
    }

    #[test]
    fn wrong_version_is_rejected() {
        let env = Env::default();
        let payload = encode_deposit_payload(&env, &BytesN::from_array(&env, &[1u8; 32]), 5);
        let mut raw = [0u8; DEPOSIT_PAYLOAD_LEN];
        payload.copy_into_slice(&mut raw);
        raw[0] = DEPOSIT_PAYLOAD_VERSION + 1;
        let future = Bytes::from_slice(&env, &raw);
        assert_eq!(
            parse_deposit_payload(&env, &future),
            Err(Error::InvalidProof)
        );
    }

    #[test]
    fn non_positive_amount_is_rejected() {
        let env = Env::default();
        let payload = encode_deposit_payload(&env, &BytesN::from_array(&env, &[1u8; 32]), 0);
        assert_eq!(
            parse_deposit_payload(&env, &payload),
            Err(Error::InvalidAmount)
        );
    }
}
