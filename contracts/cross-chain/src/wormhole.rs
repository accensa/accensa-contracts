//! Wormhole Verified Action Approval (VAA) parsing and guardian verification (issue #457).
//!
//! Parses Wormhole VAA binary envelopes and verifies secp256k1 signatures
//! produced by the Wormhole Guardian network to read verifiable cross-chain
//! state without centralized oracles.

use accensa_common::Error;
use soroban_sdk::{contracttype, Bytes, BytesN, Env, Vec};

/// 20-byte Ethereum-style address representing a Wormhole Guardian.
pub type GuardianAddress = BytesN<20>;

/// Guardian set stored on-chain, against which VAAs are verified.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuardianSet {
    /// Monotonically increasing index of this guardian set.
    pub index: u32,
    /// List of guardian public key addresses (20-byte Ethereum format).
    pub keys: Vec<GuardianAddress>,
}

impl GuardianSet {
    /// Minimum signatures required for a valid VAA: strictly > 2/3 of guardians.
    pub fn quorum(&self) -> u32 {
        (self.keys.len() * 2) / 3 + 1
    }
}

/// A single guardian's signature over the VAA body.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuardianSignature {
    /// Index of the guardian in the active GuardianSet.
    pub guardian_index: u32,
    /// 64-byte compact ECDSA signature: r (32 bytes) || s (32 bytes).
    pub signature: BytesN<64>,
    /// Recovery ID (0, 1, 27, or 28).
    pub recovery_id: u32,
}

/// Parsed body of a Wormhole VAA containing cross-chain state data.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaaBody {
    /// Unix timestamp when the VAA was emitted.
    pub timestamp: u32,
    /// Nonce set by the emitter.
    pub nonce: u32,
    /// Emitter chain ID (Wormhole chain format).
    pub emitter_chain: u32,
    /// Emitter address on the source chain (32-byte left-padded format).
    pub emitter_address: BytesN<32>,
    /// Monotonically increasing sequence number from the emitter.
    pub sequence: u64,
    /// Consistency level required by the emitter (e.g. finality confirmations).
    pub consistency_level: u32,
    /// Raw message payload carried by the VAA.
    pub payload: Bytes,
}

/// Fully parsed VAA structure including header, signatures, and body.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedVaa {
    pub version: u32,
    pub guardian_set_index: u32,
    pub signatures: Vec<GuardianSignature>,
    pub body: VaaBody,
    pub body_bytes: Bytes,
}

/// Parse a raw binary Wormhole VAA buffer into structured components.
pub fn parse_vaa(env: &Env, vaa_bytes: &Bytes) -> Result<ParsedVaa, Error> {
    let len = vaa_bytes.len();
    // Minimum VAA length: 1 (version) + 4 (guardian_set_index) + 1 (len_signatures) + 51 (min body) = 57 bytes
    if len < 57 {
        return Err(Error::InvalidProof);
    }

    let mut header = [0u8; 6];
    vaa_bytes.slice(0..6).copy_into_slice(&mut header);

    let version = header[0] as u32;
    if version != 1 {
        return Err(Error::InvalidProof);
    }

    let guardian_set_index = u32::from_be_bytes([header[1], header[2], header[3], header[4]]);
    let num_signatures = header[5] as usize;

    let signatures_end = 6 + num_signatures * 66;
    if (len as usize) < signatures_end + 51 {
        return Err(Error::InvalidProof);
    }

    let mut signatures = Vec::new(env);
    let mut sig_buf = [0u8; 66];
    for i in 0..num_signatures {
        let start = (6 + i * 66) as u32;
        vaa_bytes
            .slice(start..start + 66)
            .copy_into_slice(&mut sig_buf);

        let guardian_index = sig_buf[0] as u32;
        let mut sig_bytes = [0u8; 64];
        sig_bytes.copy_from_slice(&sig_buf[1..65]);
        let recovery_id = sig_buf[65] as u32;

        signatures.push_back(GuardianSignature {
            guardian_index,
            signature: BytesN::from_array(env, &sig_bytes),
            recovery_id,
        });
    }

    // Body parsing
    let mut body_header = [0u8; 51];
    vaa_bytes
        .slice((signatures_end as u32)..(signatures_end as u32 + 51))
        .copy_into_slice(&mut body_header);

    let timestamp = u32::from_be_bytes([
        body_header[0],
        body_header[1],
        body_header[2],
        body_header[3],
    ]);
    let nonce = u32::from_be_bytes([
        body_header[4],
        body_header[5],
        body_header[6],
        body_header[7],
    ]);
    let emitter_chain = u16::from_be_bytes([body_header[8], body_header[9]]) as u32;

    let mut emitter_addr = [0u8; 32];
    emitter_addr.copy_from_slice(&body_header[10..42]);

    let sequence = u64::from_be_bytes([
        body_header[42],
        body_header[43],
        body_header[44],
        body_header[45],
        body_header[46],
        body_header[47],
        body_header[48],
        body_header[49],
    ]);
    let consistency_level = body_header[50] as u32;

    let payload = vaa_bytes.slice((signatures_end as u32 + 51)..len);
    let body_bytes = vaa_bytes.slice(signatures_end as u32..len);

    let body = VaaBody {
        timestamp,
        nonce,
        emitter_chain,
        emitter_address: BytesN::from_array(env, &emitter_addr),
        sequence,
        consistency_level,
        payload,
    };

    Ok(ParsedVaa {
        version,
        guardian_set_index,
        signatures,
        body,
        body_bytes,
    })
}

/// Compute the double-keccak256 hash digest of the VAA body signed by guardians.
pub fn hash_vaa_body(env: &Env, body_bytes: &Bytes) -> soroban_sdk::crypto::Hash<32> {
    let h1 = env.crypto().keccak256(body_bytes);
    let h1_bytes: Bytes = h1.into();
    env.crypto().keccak256(&h1_bytes)
}

/// Recover the 20-byte Ethereum-style address from a 65-byte uncompressed secp256k1 public key.
pub fn pubkey_to_address(env: &Env, pubkey: &BytesN<65>) -> GuardianAddress {
    // Uncompressed secp256k1 public key format: 0x04 || X (32 bytes) || Y (32 bytes)
    // Ethereum address: last 20 bytes of keccak256(pubkey[1..65])
    let mut unprefixed = [0u8; 64];
    let full = pubkey.to_array();
    unprefixed.copy_from_slice(&full[1..65]);

    let raw = Bytes::from_slice(env, &unprefixed);
    let hash = env.crypto().keccak256(&raw);
    let hash_bytes = hash.to_array();

    let mut addr = [0u8; 20];
    addr.copy_from_slice(&hash_bytes[12..32]);
    BytesN::from_array(env, &addr)
}

/// Verify all guardian signatures against the specified `GuardianSet`.
pub fn verify_vaa(env: &Env, vaa: &ParsedVaa, guardian_set: &GuardianSet) -> Result<(), Error> {
    if vaa.guardian_set_index != guardian_set.index {
        return Err(Error::Unauthorized);
    }

    let quorum = guardian_set.quorum();
    if vaa.signatures.len() < quorum {
        return Err(Error::InvalidProof);
    }

    let digest = hash_vaa_body(env, &vaa.body_bytes);
    let total_guardians = guardian_set.keys.len();

    let mut last_guardian_idx: i64 = -1;

    for i in 0..vaa.signatures.len() {
        let sig = vaa.signatures.get(i).unwrap();
        let idx = sig.guardian_index as i64;

        // Signatures must be strictly ascending by guardian_index (prevents duplicates)
        if idx <= last_guardian_idx || (idx as u32) >= total_guardians {
            return Err(Error::InvalidSignature);
        }
        last_guardian_idx = idx;

        let rec_id = if sig.recovery_id >= 27 {
            sig.recovery_id - 27
        } else {
            sig.recovery_id
        };

        if rec_id > 1 {
            return Err(Error::InvalidSignature);
        }

        let recovered_pubkey = env
            .crypto()
            .secp256k1_recover(&digest, &sig.signature, rec_id);

        let recovered_addr = pubkey_to_address(env, &recovered_pubkey);
        let expected_addr = guardian_set.keys.get(sig.guardian_index).unwrap();

        if recovered_addr != expected_addr {
            return Err(Error::InvalidSignature);
        }
    }

    Ok(())
}
