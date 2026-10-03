//! Outbound withdrawal bridging requests for cross-chain settlement (issue #456).
//!
//! Allows merchants and users to withdraw their settled Soroban balances to an
//! EVM destination chain. The contract burns the wrapped asset on Soroban and
//! emits a standardized cross-chain bridging event payload for external relayers
//! to process and mint/unlock on the destination EVM network.

use accensa_common::Error;
use soroban_sdk::{contracttype, symbol_short, Address, BytesN, Env, Symbol};

/// EVM address is a 20-byte identifier.
pub type EvmAddress = BytesN<20>;

/// Standard event topics for cross-chain bridge events.
pub const BRIDGE_TOPIC: Symbol = symbol_short!("bridge");
pub const WITHDRAW_ACTION: Symbol = symbol_short!("withdraw");

/// Standardized outbound bridge payload emitted for external relayers.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutboundBridgePayload {
    /// Monotonically increasing sequence number for ordering and replay protection.
    pub sequence: u64,
    /// Soroban address of the merchant / caller whose wrapped assets were burned.
    pub sender: Address,
    /// 20-byte destination recipient address on the EVM chain.
    pub evm_recipient: EvmAddress,
    /// Amount of tokens burned on Soroban to be bridged.
    pub amount: i128,
    /// Target EVM chain ID (e.g., 1 for Ethereum, 137 for Polygon, 8453 for Base).
    pub destination_chain_id: u32,
    /// Token contract address burned on Soroban.
    pub token: Address,
    /// Ledger timestamp when the withdrawal was initiated.
    pub timestamp: u64,
}

/// Executes an outbound withdrawal to EVM: burns the wrapped asset from `caller`
/// and emits the standardized bridging payload for relayers.
#[allow(deprecated)]
pub fn execute_withdrawal_to_evm(
    env: &Env,
    caller: &Address,
    token: &Address,
    evm_address: &EvmAddress,
    amount: i128,
    destination_chain_id: u32,
    sequence: u64,
) -> Result<OutboundBridgePayload, Error> {
    if amount <= 0 {
        return Err(Error::InvalidAmount);
    }

    caller.require_auth();

    // Burn wrapped asset on Soroban
    soroban_sdk::token::Client::new(env, token).burn(caller, &amount);

    let payload = OutboundBridgePayload {
        sequence,
        sender: caller.clone(),
        evm_recipient: evm_address.clone(),
        amount,
        destination_chain_id,
        token: token.clone(),
        timestamp: env.ledger().timestamp(),
    };

    // Emit standardized bridging event payload for external relayers
    env.events()
        .publish((BRIDGE_TOPIC, WITHDRAW_ACTION, sequence), payload.clone());

    Ok(payload)
}
