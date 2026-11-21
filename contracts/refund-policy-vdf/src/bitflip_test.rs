//! Exhaustive single-bit-flip coverage for the VDF verifier (issue #989).
//!
//! `verify_vdf` accepts a proof only when `pi^l * x^r == y (mod N)`, so every
//! one of the 2048 bits of the `output || witness` bundle matters. The existing
//! suite probes just `output[127]` and `witness[127]`; this test flips each bit
//! in turn, asserts the typed rejection, then restores it, so a truncated
//! comparison or a limb-only range check cannot pass CI.
//!
//! `verify_vdf`, `derive_challenge` and the prover helpers are crate-private,
//! so this lives beside `test` as a `#[cfg(test)]` module rather than in
//! `tests/`.

use crate::test::{challenge_for, eval_vdf, pack};
use crate::VdfPolicy;
use accensa_common::{Error, PolicyContext, RefundPolicyClient, VdfPolicyParams};
use soroban_sdk::{testutils::EnvTestConfig, xdr::ToXdr, Bytes, BytesN, Env};

/// `2^300 > l` for the 128-bit Fiat-Shamir prime, so the honest witness is
/// non-trivial (the floor exponent is large, not zero).
const DELAY: u32 = 300;

fn test_env() -> Env {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    env
}

fn params(env: &Env, delay: u32) -> Bytes {
    VdfPolicyParams { delay }.to_xdr(env)
}

fn ctx(_env: &Env, payment_ref: &BytesN<32>, proof: BytesN<256>) -> PolicyContext {
    PolicyContext {
        payment_ref: payment_ref.clone(),
        amount: 100,
        paid_at_ledger: 0,
        current_ledger: 0,
        timestamp: 0,
        vdf_proof: Some(proof),
    }
}

#[test]
fn any_single_flipped_proof_bit_is_rejected() {
    let env = test_env();
    let id = env.register(VdfPolicy, ());
    let client = RefundPolicyClient::new(&env, &id);
    let payment_ref = BytesN::from_array(&env, &[1u8; 32]);
    let challenge = challenge_for(&env, &payment_ref);
    let (output, witness) = eval_vdf(&env, &challenge, DELAY);

    // Control: the untampered pair verifies before any bit is flipped.
    assert_eq!(
        client.try_evaluate(
            &params(&env, DELAY),
            &ctx(&env, &payment_ref, pack(&env, &output, &witness)),
        ),
        Ok(Ok(()))
    );

    // 1024 bits of the output half (`y`).
    let mut output_flips = 0u32;
    for bit in 0..1024u32 {
        let mut tampered = output;
        tampered[(bit / 8) as usize] ^= 1u8 << (bit % 8);
        assert_eq!(
            client.try_evaluate(
                &params(&env, DELAY),
                &ctx(&env, &payment_ref, pack(&env, &tampered, &witness)),
            ),
            Err(Ok(Error::InvalidVdfProof)),
            "flipped output bit {bit} was accepted"
        );
        output_flips += 1;
    }

    // 1024 bits of the witness half (`pi`).
    let mut witness_flips = 0u32;
    for bit in 0..1024u32 {
        let mut tampered = witness;
        tampered[(bit / 8) as usize] ^= 1u8 << (bit % 8);
        assert_eq!(
            client.try_evaluate(
                &params(&env, DELAY),
                &ctx(&env, &payment_ref, pack(&env, &output, &tampered)),
            ),
            Err(Ok(Error::InvalidVdfProof)),
            "flipped witness bit {bit} was accepted"
        );
        witness_flips += 1;
    }

    assert_eq!((output_flips, witness_flips), (1024, 1024));
}
