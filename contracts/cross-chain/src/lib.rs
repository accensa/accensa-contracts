#![no_std]

use accensa_common::Error;
pub use layerzero::{
    encode_dispute_payload, parse_dispute_payload, DisputeResolution, DisputeResolvedEvent,
    PeerAddress,
};
pub use outbound::{EvmAddress, OutboundBridgePayload};
use soroban_sdk::{contract, contractimpl, contracttype, Address, Bytes, BytesN, Env, String};
pub use wormhole::{
    hash_vaa_body, parse_vaa, pubkey_to_address, verify_vaa, GuardianAddress, GuardianSet,
    GuardianSignature, ParsedVaa, VaaBody,
};

pub mod axelar;
pub mod layerzero;
pub mod outbound;
pub mod wormhole;

#[cfg(test)]
mod axelar_test;
#[cfg(test)]
mod layerzero_test;
#[cfg(test)]
mod test;
#[cfg(test)]
mod wormhole_test;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    Admin,
    Token,
    DestinationChainId,
    NextSequence,
    Paused,
    GuardianSet(u32),
    CurrentGuardianSetIndex,
    /// Instance: the admin-configured LayerZero endpoint allowed to deliver
    /// packets to `lz_receive` (issue #455).
    LzEndpoint,
    /// Instance: the admin-configured Axelar gateway allowed to deliver
    /// deposits to `axelar_execute` (issue #454).
    AxelarGateway,
}

#[contract]
pub struct CrossChainBridge;

#[contractimpl]
impl CrossChainBridge {
    /// Initialize the cross-chain bridge with admin, wrapped token address,
    /// and destination EVM chain ID.
    pub fn initialize(
        env: Env,
        admin: Address,
        token: Address,
        destination_chain_id: u32,
    ) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage()
            .instance()
            .set(&DataKey::DestinationChainId, &destination_chain_id);
        env.storage().instance().set(&DataKey::NextSequence, &1u64);
        env.storage().instance().set(&DataKey::Paused, &false);
        env.storage()
            .instance()
            .set(&DataKey::CurrentGuardianSetIndex, &0u32);

        Ok(())
    }

    /// Set an active Wormhole GuardianSet (admin only).
    pub fn set_guardian_set(env: Env, admin: Address, set: GuardianSet) -> Result<(), Error> {
        admin.require_auth();
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        if admin != stored_admin {
            return Err(Error::Unauthorized);
        }

        let idx = set.index;
        env.storage()
            .instance()
            .set(&DataKey::GuardianSet(idx), &set);
        env.storage()
            .instance()
            .set(&DataKey::CurrentGuardianSetIndex, &idx);

        Ok(())
    }

    /// Get a stored GuardianSet by index.
    pub fn get_guardian_set(env: Env, index: u32) -> Option<GuardianSet> {
        env.storage().instance().get(&DataKey::GuardianSet(index))
    }

    /// Get the current active GuardianSet index.
    pub fn get_current_guardian_set_index(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::CurrentGuardianSetIndex)
            .unwrap_or(0)
    }

    /// Register the Axelar gateway allowed to deliver cross-chain deposits
    /// (admin only, issue #454).
    pub fn set_axelar_gateway(env: Env, admin: Address, gateway: Address) -> Result<(), Error> {
        admin.require_auth();
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        if admin != stored_admin {
            return Err(Error::Unauthorized);
        }
        env.storage()
            .instance()
            .set(&DataKey::AxelarGateway, &gateway);
        Ok(())
    }

    /// Return the configured Axelar gateway, if any.
    pub fn get_axelar_gateway(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::AxelarGateway)
    }

    /// Execute an inbound Axelar message, crediting the bridged deposit
    /// (issue #454).
    ///
    /// Only the registered gateway may deliver, the gateway must confirm the
    /// message via `validate_message`, and each `message_id` is credited at most
    /// once. Returns the recipient's running credited total.
    pub fn axelar_execute(
        env: Env,
        gateway: Address,
        source_chain: String,
        message_id: String,
        source_address: String,
        payload: Bytes,
    ) -> Result<i128, Error> {
        if env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::Paused)
            .unwrap_or(false)
        {
            return Err(Error::Paused);
        }

        let configured: Address = env
            .storage()
            .instance()
            .get(&DataKey::AxelarGateway)
            .ok_or(Error::NotInitialized)?;

        gateway.require_auth();
        if gateway != configured {
            return Err(Error::Unauthorized);
        }

        let payload_hash: BytesN<32> = env.crypto().sha256(&payload).into();
        let validated = axelar::AxelarGatewayClient::new(&env, &gateway).validate_message(
            &source_chain,
            &message_id,
            &source_address,
            &payload_hash,
        );
        if !validated {
            return Err(Error::InvalidProof);
        }

        let deposit = axelar::parse_deposit_payload(&env, &payload)?;
        axelar::record_deposit(&env, deposit, source_chain, message_id)
    }

    /// Verify a Wormhole VAA and extract its cross-chain payload.
    pub fn verify_and_parse_vaa(env: Env, vaa_bytes: Bytes) -> Result<VaaBody, Error> {
        let parsed = wormhole::parse_vaa(&env, &vaa_bytes)?;
        let guardian_set = Self::get_guardian_set(env.clone(), parsed.guardian_set_index)
            .ok_or(Error::RootNotFound)?;
        wormhole::verify_vaa(&env, &parsed, &guardian_set)?;
        Ok(parsed.body)
    }

    /// Withdraw settled Soroban balance directly to an EVM chain by burning
    /// the wrapped asset and emitting a bridge request (issue #456).
    pub fn withdraw_to_evm(
        env: Env,
        caller: Address,
        evm_address: EvmAddress,
        amount: i128,
    ) -> Result<u64, Error> {
        if Self::is_paused(&env) {
            return Err(Error::Paused);
        }

        let token: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(Error::NotInitialized)?;

        let destination_chain_id: u32 = env
            .storage()
            .instance()
            .get(&DataKey::DestinationChainId)
            .ok_or(Error::NotInitialized)?;

        let sequence: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextSequence)
            .unwrap_or(1);

        outbound::execute_withdrawal_to_evm(
            &env,
            &caller,
            &token,
            &evm_address,
            amount,
            destination_chain_id,
            sequence,
        )?;

        env.storage()
            .instance()
            .set(&DataKey::NextSequence, &(sequence + 1));

        Ok(sequence)
    }

    /// Pause the bridge contract (admin only).
    pub fn pause(env: Env, admin: Address) -> Result<(), Error> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;
        env.storage().instance().set(&DataKey::Paused, &true);
        Ok(())
    }

    /// Unpause the bridge contract (admin only).
    pub fn unpause(env: Env, admin: Address) -> Result<(), Error> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;
        env.storage().instance().set(&DataKey::Paused, &false);
        Ok(())
    }

    /// Verify `admin` matches the stored admin (call after `require_auth`).
    fn require_admin(env: &Env, admin: &Address) -> Result<(), Error> {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        if admin != &stored_admin {
            return Err(Error::Unauthorized);
        }
        Ok(())
    }

    pub fn is_paused(env: &Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }

    pub fn get_token(env: Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(Error::NotInitialized)
    }

    pub fn get_destination_chain_id(env: Env) -> Result<u32, Error> {
        env.storage()
            .instance()
            .get(&DataKey::DestinationChainId)
            .ok_or(Error::NotInitialized)
    }

    pub fn get_next_sequence(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::NextSequence)
            .unwrap_or(1)
    }

    pub fn get_admin(env: Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)
    }

    // ── LayerZero omnichain dispute bridging (issue #455) ────────────────

    /// Register (admin) the LayerZero endpoint contract allowed to deliver
    /// packets to [`lz_receive`](Self::lz_receive). Calling it again with a
    /// different address rotates the trusted endpoint.
    pub fn set_layerzero_endpoint(
        env: Env,
        admin: Address,
        endpoint: Address,
    ) -> Result<(), Error> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;
        env.storage()
            .instance()
            .set(&DataKey::LzEndpoint, &endpoint);
        Ok(())
    }

    /// The registered LayerZero endpoint, if any. `lz_receive` fails closed
    /// with [`Error::NotInitialized`] until one is configured.
    pub fn get_layerzero_endpoint(env: Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::LzEndpoint)
            .ok_or(Error::NotInitialized)
    }

    /// Register (admin) a trusted peer contract on a remote chain that may
    /// deliver dispute resolutions. Only packets whose
    /// `(src_eid, sender_address)` names a registered peer are accepted.
    pub fn set_trusted_peer(env: Env, admin: Address, peer: PeerAddress) -> Result<(), Error> {
        admin.require_auth();
        Self::require_admin(&env, &admin)?;
        let key = layerzero::DataKey::TrustedPeer(peer.chain_id, peer.sender.clone());
        env.storage().persistent().set(&key, &true);
        Ok(())
    }

    /// Whether `(src_eid, sender)` is a registered trusted peer.
    pub fn is_trusted_peer(env: Env, src_eid: u32, sender: BytesN<32>) -> bool {
        env.storage()
            .persistent()
            .has(&layerzero::DataKey::TrustedPeer(src_eid, sender))
    }

    /// Highest accepted LayerZero packet nonce for a peer channel (`0` when
    /// nothing has been delivered yet).
    pub fn get_peer_nonce(env: Env, src_eid: u32, sender: BytesN<32>) -> u64 {
        env.storage()
            .persistent()
            .get(&layerzero::DataKey::PeerNonce(src_eid, sender))
            .unwrap_or(0)
    }

    /// Whether the dispute has already been settled through the bridge.
    pub fn is_dispute_settled(env: Env, dispute_id: BytesN<32>) -> bool {
        env.storage()
            .persistent()
            .has(&layerzero::DataKey::DisputeSettled(dispute_id))
    }

    /// Receive a dispute resolution from a remote chain through the
    /// registered LayerZero endpoint.
    ///
    /// The endpoint calls this while processing its `lzReceive` message;
    /// the `endpoint` argument names the endpoint contract and its
    /// `require_auth` authenticates the delivery (in tests, the real auth
    /// path is proven with a mock endpoint contract calling in). The
    /// Soroban contract validates what LayerZero cannot guarantee about the
    /// payload itself: that the packet names a trusted peer, carries a
    /// fresh, strictly-advancing nonce, encodes a well-formed dispute
    /// payload, and settles a dispute that has never been settled before.
    /// See [`layerzero`] for the threat model.
    pub fn lz_receive(
        env: Env,
        endpoint: Address,
        src_eid: u32,
        sender: BytesN<32>,
        nonce: u64,
        payload: Bytes,
    ) -> Result<DisputeResolution, Error> {
        // Only the registered LayerZero endpoint may deliver packets. The
        // caller must demonstrate the endpoint's identity by providing its
        // authorization — an unconfigured endpoint fails closed.
        endpoint.require_auth();
        let registered: Address = env
            .storage()
            .instance()
            .get(&DataKey::LzEndpoint)
            .ok_or(Error::NotInitialized)?;
        if endpoint != registered {
            return Err(Error::Unauthorized);
        }

        if Self::is_paused(&env) {
            return Err(Error::Paused);
        }

        // The packet must come from a peer the admin has trusted on this
        // source chain. Unknown peers are rejected, not merely ignored.
        if !Self::is_trusted_peer(env.clone(), src_eid, sender.clone()) {
            return Err(Error::Unauthorized);
        }

        // Packet nonces only move forward per peer channel; a replayed or
        // stale packet cannot be re-processed.
        let last = Self::get_peer_nonce(env.clone(), src_eid, sender.clone());
        if nonce <= last {
            return Err(Error::StaleState);
        }

        // Fully bounds-check the wire format before touching state.
        let mut resolution = layerzero::parse_dispute_payload(&env, &payload)?;
        resolution.src_chain_id = src_eid;
        resolution.peer = sender;

        layerzero::record_dispute_resolution(&env, resolution.clone(), nonce)?;

        Ok(resolution)
    }
}
