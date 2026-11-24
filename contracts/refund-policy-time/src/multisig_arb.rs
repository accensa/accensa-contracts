//! Multi-signature dispute arbitration for high-value claims.
//!
//! Require disputes involving > $10,000 to be settled by a 3-of-5 multisig
//! committee of arbitrators rather than a single decentralized node.

use accensa_common::Error;
use soroban_sdk::{contractevent, contracttype, Bytes, BytesN, Env, Vec};

/// Default high-value dispute threshold ($10,000).
pub const DEFAULT_HIGH_VALUE_THRESHOLD: i128 = 10_000;
/// Default arbitration committee size (5 arbitrators).
pub const DEFAULT_COMMITTEE_SIZE: u32 = 5;
/// Default required quorum for high-value disputes (3 of 5).
pub const DEFAULT_QUORUM: u32 = 3;
/// Domain separator for dispute arbitration signatures.
pub const ARBITRATION_DOMAIN: &[u8] = b"accensa-dispute-arbitration-v1";

/// The decision outcome of an arbitration.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DisputeOutcome {
    Admit = 1,
    Deny = 2,
}

/// Committee configuration specifying the arbitrator keys, quorum, and threshold.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArbitrationCommittee {
    /// Array of 32-byte Ed25519 public keys of recognized committee arbitrators.
    pub arbitrator_keys: Vec<BytesN<32>>,
    /// Quorum threshold required for high-value disputes (e.g. 3 of 5).
    pub quorum: u32,
    /// Value threshold above which multisig committee arbitration is mandatory.
    pub high_value_threshold: i128,
}

impl ArbitrationCommittee {
    /// Constructs and validates a new `ArbitrationCommittee`.
    pub fn new(
        arbitrator_keys: Vec<BytesN<32>>,
        quorum: u32,
        high_value_threshold: i128,
    ) -> Result<Self, Error> {
        if quorum == 0 || quorum > arbitrator_keys.len() {
            return Err(Error::InvalidPolicyParams);
        }
        // Ensure no duplicate arbitrator keys exist in the committee.
        for i in 0..arbitrator_keys.len() {
            let k_i = arbitrator_keys.get(i).unwrap();
            for j in (i + 1)..arbitrator_keys.len() {
                let k_j = arbitrator_keys.get(j).unwrap();
                if k_i == k_j {
                    return Err(Error::InvalidPolicyParams);
                }
            }
        }
        Ok(Self {
            arbitrator_keys,
            quorum,
            high_value_threshold,
        })
    }
}

/// The payload signed by arbitrators to resolve a dispute.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolutionPayload {
    pub payment_ref: BytesN<32>,
    pub amount: i128,
    pub outcome: DisputeOutcome,
    pub timestamp: u64,
}

/// A signature submitted by an individual arbitrator.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArbitratorSignature {
    pub arbitrator_key: BytesN<32>,
    pub signature: BytesN<64>,
}

/// Audit event emitted when a dispute is arbitrated.
#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeArbitrated {
    #[topic]
    pub payment_ref: BytesN<32>,
    pub outcome: DisputeOutcome,
    pub amount: i128,
    pub approvals: u32,
}

/// Storage key for persisting committee configuration in instance storage.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    Committee,
}

/// Store arbitration committee in instance storage.
pub fn store_committee(env: &Env, committee: &ArbitrationCommittee) {
    env.storage().instance().set(&DataKey::Committee, committee);
}

/// Load arbitration committee from instance storage if configured.
pub fn load_committee(env: &Env) -> Option<ArbitrationCommittee> {
    env.storage().instance().get(&DataKey::Committee)
}

/// Checks whether a dispute amount qualifies as a high-value dispute.
pub fn is_high_value(amount: i128, threshold: i128) -> bool {
    amount > threshold
}

/// Computes the canonical 32-byte digest of a resolution payload for signing.
pub fn compute_payload_digest(env: &Env, payload: &ResolutionPayload) -> BytesN<32> {
    let mut buf = Bytes::new(env);
    buf.extend_from_slice(ARBITRATION_DOMAIN);
    buf.extend_from_slice(&payload.payment_ref.to_array());
    buf.extend_from_slice(&payload.amount.to_be_bytes());
    buf.extend_from_slice(&(payload.outcome as u32).to_be_bytes());
    buf.extend_from_slice(&payload.timestamp.to_be_bytes());
    env.crypto().sha256(&buf).into()
}

/// Tally signatures on the resolution payload and verify threshold requirements.
///
/// Disputes involving amounts strictly greater than `high_value_threshold` require
/// at least `committee.quorum` (e.g. 3 of 5) valid distinct arbitrator signatures.
/// Lower-value disputes require at least 1 valid arbitrator signature.
pub fn tally_and_verify_resolution(
    env: &Env,
    committee: &ArbitrationCommittee,
    payload: &ResolutionPayload,
    signatures: &Vec<ArbitratorSignature>,
) -> Result<DisputeOutcome, Error> {
    if payload.amount <= 0 {
        return Err(Error::InvalidAmount);
    }

    let required_quorum = if is_high_value(payload.amount, committee.high_value_threshold) {
        committee.quorum
    } else {
        1
    };

    let digest = compute_payload_digest(env, payload);
    let msg = Bytes::from(digest);

    let mut counted: Vec<BytesN<32>> = Vec::new(env);
    let mut tallied = 0u32;

    for sig in signatures.iter() {
        // Ensure signer is a member of the committee.
        let mut is_member = false;
        for member in committee.arbitrator_keys.iter() {
            if member == sig.arbitrator_key {
                is_member = true;
                break;
            }
        }
        if !is_member {
            continue;
        }

        // Prevent duplicate signatures from the same arbitrator.
        let mut already_counted = false;
        for seen in counted.iter() {
            if seen == sig.arbitrator_key {
                already_counted = true;
                break;
            }
        }
        if already_counted {
            continue;
        }

        // Verify Ed25519 signature against payload digest.
        env.crypto()
            .ed25519_verify(&sig.arbitrator_key, &msg, &sig.signature);

        counted.push_back(sig.arbitrator_key);
        tallied += 1;
    }

    if tallied < required_quorum {
        return Err(Error::Unauthorized);
    }

    DisputeArbitrated {
        payment_ref: payload.payment_ref.clone(),
        outcome: payload.outcome,
        amount: payload.amount,
        approvals: tallied,
    }
    .publish(env);

    Ok(payload.outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        testutils::{EnvTestConfig, Events as _},
        vec,
    };

    fn test_env() -> Env {
        Env::new_with_config(EnvTestConfig {
            capture_snapshot_at_drop: false,
        })
    }

    // Pre-computed Ed25519 test vectors generated over:
    // ARBITRATION_DOMAIN || payment_ref ([1u8; 32]) || amount || outcome (1) || timestamp (1000)
    const PUB_1: [u8; 32] = [
        156, 38, 79, 159, 7, 249, 82, 164, 86, 81, 129, 21, 126, 11, 185, 73, 139, 112, 187, 165,
        8, 139, 224, 238, 239, 146, 87, 144, 73, 220, 42, 147,
    ];
    const SIG_HIGH_1: [u8; 64] = [
        86, 91, 253, 44, 218, 24, 205, 227, 155, 206, 12, 79, 181, 161, 133, 183, 111, 106, 87,
        245, 43, 156, 64, 216, 218, 45, 33, 239, 159, 61, 64, 14, 248, 51, 18, 161, 81, 246, 24,
        147, 163, 202, 151, 136, 60, 127, 88, 203, 101, 147, 141, 77, 251, 40, 61, 168, 154, 20,
        170, 235, 251, 5, 51, 8,
    ];
    const SIG_LOW_1: [u8; 64] = [
        121, 76, 72, 162, 228, 86, 183, 194, 27, 208, 57, 208, 135, 189, 101, 156, 170, 44, 24, 97,
        215, 200, 192, 32, 71, 12, 255, 181, 232, 136, 127, 213, 123, 219, 58, 206, 174, 105, 215,
        115, 223, 24, 195, 139, 181, 158, 202, 226, 66, 15, 34, 62, 216, 126, 199, 56, 77, 153, 53,
        132, 15, 235, 33, 11,
    ];

    const PUB_2: [u8; 32] = [
        236, 164, 195, 32, 148, 225, 174, 30, 32, 165, 123, 106, 154, 44, 247, 76, 215, 192, 195,
        72, 119, 25, 32, 168, 150, 137, 162, 249, 161, 66, 136, 127,
    ];
    const SIG_HIGH_2: [u8; 64] = [
        37, 134, 162, 236, 158, 4, 90, 79, 180, 144, 218, 177, 67, 40, 14, 6, 243, 204, 99, 25, 94,
        226, 249, 47, 248, 103, 230, 103, 224, 230, 227, 33, 188, 155, 250, 23, 184, 152, 39, 56,
        49, 214, 158, 237, 109, 113, 175, 155, 219, 86, 189, 142, 89, 22, 211, 144, 185, 27, 236,
        255, 152, 255, 28, 11,
    ];

    const PUB_3: [u8; 32] = [
        224, 203, 221, 171, 231, 19, 255, 185, 29, 10, 94, 32, 190, 106, 147, 182, 169, 19, 136, 7,
        147, 96, 183, 42, 159, 93, 151, 134, 35, 124, 172, 123,
    ];
    const SIG_HIGH_3: [u8; 64] = [
        237, 177, 176, 250, 75, 103, 46, 226, 17, 32, 156, 180, 237, 227, 219, 248, 219, 232, 143,
        205, 148, 177, 51, 76, 28, 152, 152, 107, 54, 255, 33, 84, 239, 196, 115, 182, 247, 225,
        11, 175, 254, 18, 149, 181, 74, 208, 164, 60, 112, 94, 241, 167, 79, 133, 107, 43, 197,
        135, 160, 222, 28, 219, 177, 4,
    ];

    const PUB_4: [u8; 32] = [
        65, 129, 219, 130, 193, 59, 205, 142, 148, 143, 55, 79, 27, 74, 153, 185, 213, 146, 173,
        77, 225, 104, 224, 144, 201, 67, 81, 127, 44, 15, 61, 61,
    ];
    const SIG_HIGH_4: [u8; 64] = [
        170, 69, 200, 206, 13, 20, 183, 153, 174, 226, 56, 96, 18, 184, 211, 175, 163, 163, 200,
        190, 192, 11, 235, 122, 0, 208, 242, 240, 208, 25, 152, 163, 167, 180, 238, 213, 36, 200,
        23, 40, 141, 108, 158, 124, 220, 235, 54, 108, 139, 136, 197, 67, 150, 177, 219, 219, 244,
        14, 76, 186, 120, 104, 72, 15,
    ];

    const PUB_5: [u8; 32] = [
        7, 121, 29, 163, 70, 37, 26, 125, 174, 223, 251, 234, 118, 164, 32, 11, 154, 91, 123, 39,
        91, 112, 79, 124, 110, 164, 206, 137, 141, 233, 108, 22,
    ];
    const SIG_HIGH_5: [u8; 64] = [
        213, 217, 229, 80, 234, 136, 3, 143, 248, 86, 79, 177, 228, 59, 160, 187, 46, 32, 167, 43,
        87, 136, 0, 197, 121, 11, 120, 78, 1, 227, 60, 138, 245, 235, 154, 76, 74, 225, 128, 9, 35,
        16, 49, 149, 46, 139, 11, 47, 48, 34, 191, 41, 222, 151, 74, 81, 141, 165, 0, 170, 232,
        158, 181, 0,
    ];

    fn test_committee(env: &Env) -> ArbitrationCommittee {
        let keys = vec![
            env,
            BytesN::from_array(env, &PUB_1),
            BytesN::from_array(env, &PUB_2),
            BytesN::from_array(env, &PUB_3),
            BytesN::from_array(env, &PUB_4),
            BytesN::from_array(env, &PUB_5),
        ];
        ArbitrationCommittee::new(keys, DEFAULT_QUORUM, DEFAULT_HIGH_VALUE_THRESHOLD).unwrap()
    }

    #[test]
    fn test_value_threshold_logic() {
        assert!(!is_high_value(5_000, DEFAULT_HIGH_VALUE_THRESHOLD));
        assert!(!is_high_value(10_000, DEFAULT_HIGH_VALUE_THRESHOLD));
        assert!(is_high_value(10_001, DEFAULT_HIGH_VALUE_THRESHOLD));
        assert!(is_high_value(15_000, DEFAULT_HIGH_VALUE_THRESHOLD));
    }

    #[test]
    fn test_committee_validation() {
        let env = test_env();
        let k1 = BytesN::from_array(&env, &PUB_1);
        let k2 = BytesN::from_array(&env, &PUB_2);

        // Zero quorum rejected
        assert_eq!(
            ArbitrationCommittee::new(vec![&env, k1.clone(), k2.clone()], 0, 10_000),
            Err(Error::InvalidPolicyParams)
        );

        // Quorum exceeding signers rejected
        assert_eq!(
            ArbitrationCommittee::new(vec![&env, k1.clone(), k2.clone()], 3, 10_000),
            Err(Error::InvalidPolicyParams)
        );

        // Duplicate keys rejected
        assert_eq!(
            ArbitrationCommittee::new(vec![&env, k1.clone(), k1.clone()], 2, 10_000),
            Err(Error::InvalidPolicyParams)
        );

        // Valid 3-of-5 committee
        let c = test_committee(&env);
        assert_eq!(c.arbitrator_keys.len(), 5);
        assert_eq!(c.quorum, 3);
        assert_eq!(c.high_value_threshold, 10_000);
    }

    #[test]
    fn test_high_value_threshold_approvals_succeeds_with_3_of_5() {
        let env = test_env();
        let committee = test_committee(&env);

        let payload = ResolutionPayload {
            payment_ref: BytesN::from_array(&env, &[1u8; 32]),
            amount: 15_000,
            outcome: DisputeOutcome::Admit,
            timestamp: 1000,
        };

        // Supply 3 valid distinct signatures
        let sigs = vec![
            &env,
            ArbitratorSignature {
                arbitrator_key: BytesN::from_array(&env, &PUB_1),
                signature: BytesN::from_array(&env, &SIG_HIGH_1),
            },
            ArbitratorSignature {
                arbitrator_key: BytesN::from_array(&env, &PUB_2),
                signature: BytesN::from_array(&env, &SIG_HIGH_2),
            },
            ArbitratorSignature {
                arbitrator_key: BytesN::from_array(&env, &PUB_3),
                signature: BytesN::from_array(&env, &SIG_HIGH_3),
            },
        ];

        let res = tally_and_verify_resolution(&env, &committee, &payload, &sigs);
        assert_eq!(res, Ok(DisputeOutcome::Admit));
    }

    #[test]
    fn test_high_value_all_five_arbitrators_succeeds_and_publishes_event() {
        let env = test_env();
        let committee = test_committee(&env);

        let payload = ResolutionPayload {
            payment_ref: BytesN::from_array(&env, &[1u8; 32]),
            amount: 15_000,
            outcome: DisputeOutcome::Admit,
            timestamp: 1000,
        };

        let sigs = vec![
            &env,
            ArbitratorSignature {
                arbitrator_key: BytesN::from_array(&env, &PUB_1),
                signature: BytesN::from_array(&env, &SIG_HIGH_1),
            },
            ArbitratorSignature {
                arbitrator_key: BytesN::from_array(&env, &PUB_2),
                signature: BytesN::from_array(&env, &SIG_HIGH_2),
            },
            ArbitratorSignature {
                arbitrator_key: BytesN::from_array(&env, &PUB_3),
                signature: BytesN::from_array(&env, &SIG_HIGH_3),
            },
            ArbitratorSignature {
                arbitrator_key: BytesN::from_array(&env, &PUB_4),
                signature: BytesN::from_array(&env, &SIG_HIGH_4),
            },
            ArbitratorSignature {
                arbitrator_key: BytesN::from_array(&env, &PUB_5),
                signature: BytesN::from_array(&env, &SIG_HIGH_5),
            },
        ];

        let contract_id = env.register(crate::TimePolicy, ());
        env.as_contract(&contract_id, || {
            let res = tally_and_verify_resolution(&env, &committee, &payload, &sigs);
            assert_eq!(res, Ok(DisputeOutcome::Admit));
            assert!(!env.events().all().events().is_empty());
        });
    }

    #[test]
    fn test_high_value_sub_threshold_rejections() {
        let env = test_env();
        let committee = test_committee(&env);

        let payload = ResolutionPayload {
            payment_ref: BytesN::from_array(&env, &[1u8; 32]),
            amount: 15_000,
            outcome: DisputeOutcome::Admit,
            timestamp: 1000,
        };

        // Only 2 of 5 signatures (sub-threshold for high-value dispute)
        let sigs = vec![
            &env,
            ArbitratorSignature {
                arbitrator_key: BytesN::from_array(&env, &PUB_1),
                signature: BytesN::from_array(&env, &SIG_HIGH_1),
            },
            ArbitratorSignature {
                arbitrator_key: BytesN::from_array(&env, &PUB_2),
                signature: BytesN::from_array(&env, &SIG_HIGH_2),
            },
        ];

        let res = tally_and_verify_resolution(&env, &committee, &payload, &sigs);
        assert_eq!(res, Err(Error::Unauthorized));

        // 0 signatures
        let empty_sigs = vec![&env];
        assert_eq!(
            tally_and_verify_resolution(&env, &committee, &payload, &empty_sigs),
            Err(Error::Unauthorized)
        );
    }

    #[test]
    fn test_duplicate_signatures_do_not_count_towards_quorum() {
        let env = test_env();
        let committee = test_committee(&env);

        let payload = ResolutionPayload {
            payment_ref: BytesN::from_array(&env, &[1u8; 32]),
            amount: 15_000,
            outcome: DisputeOutcome::Admit,
            timestamp: 1000,
        };

        // Arbitrator 1 submits twice + Arbitrator 2 submits once = 3 signatures, but only 2 distinct
        let sigs = vec![
            &env,
            ArbitratorSignature {
                arbitrator_key: BytesN::from_array(&env, &PUB_1),
                signature: BytesN::from_array(&env, &SIG_HIGH_1),
            },
            ArbitratorSignature {
                arbitrator_key: BytesN::from_array(&env, &PUB_1),
                signature: BytesN::from_array(&env, &SIG_HIGH_1),
            },
            ArbitratorSignature {
                arbitrator_key: BytesN::from_array(&env, &PUB_2),
                signature: BytesN::from_array(&env, &SIG_HIGH_2),
            },
        ];

        let res = tally_and_verify_resolution(&env, &committee, &payload, &sigs);
        assert_eq!(res, Err(Error::Unauthorized));
    }

    #[test]
    fn test_sub_threshold_value_single_node_arbitration_succeeds() {
        let env = test_env();
        let committee = test_committee(&env);

        let payload = ResolutionPayload {
            payment_ref: BytesN::from_array(&env, &[1u8; 32]),
            amount: 5_000, // <= 10_000 threshold
            outcome: DisputeOutcome::Admit,
            timestamp: 1000,
        };

        // Single valid arbitrator signature suffices for sub-threshold dispute
        let sigs = vec![
            &env,
            ArbitratorSignature {
                arbitrator_key: BytesN::from_array(&env, &PUB_1),
                signature: BytesN::from_array(&env, &SIG_LOW_1),
            },
        ];

        let res = tally_and_verify_resolution(&env, &committee, &payload, &sigs);
        assert_eq!(res, Ok(DisputeOutcome::Admit));
    }

    #[test]
    fn test_state_persistence_committee() {
        let env = test_env();
        let contract_id = env.register(crate::TimePolicy, ());
        let committee = test_committee(&env);

        env.as_contract(&contract_id, || {
            assert_eq!(load_committee(&env), None);
            store_committee(&env, &committee);
            assert_eq!(load_committee(&env), Some(committee.clone()));
        });
    }

    #[test]
    fn test_invalid_amount_rejected() {
        let env = test_env();
        let committee = test_committee(&env);

        let payload = ResolutionPayload {
            payment_ref: BytesN::from_array(&env, &[1u8; 32]),
            amount: 0,
            outcome: DisputeOutcome::Admit,
            timestamp: 1000,
        };

        let sigs = vec![&env];
        assert_eq!(
            tally_and_verify_resolution(&env, &committee, &payload, &sigs),
            Err(Error::InvalidAmount)
        );
    }
}
