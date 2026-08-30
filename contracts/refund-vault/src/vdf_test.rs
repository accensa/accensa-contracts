#![cfg(test)]

extern crate std;

use super::*;
use crate::vdf;
use crypto_bigint::{
    modular::runtime_mod::{DynResidue, DynResidueParams},
    Encoding, NonZero, U1024,
};
use soroban_sdk::{
    testutils::{Address as _, EnvTestConfig, Ledger},
    token::StellarAssetClient,
    vec, Address, Bytes, BytesN, Env,
};

const FLOAT: i128 = 1_000_000;

/// An `Env` that does not write golden ledger snapshots on drop (same
/// convention as the fuzz suite: every generated case would otherwise write a
/// snapshot file).
fn test_env() -> Env {
    Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    })
}

fn setup(window: u32) -> (Env, RefundVaultClient<'static>, Address, Address) {
    let env = test_env();
    env.mock_all_auths();

    let merchant = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_admin);
    let token = sac.address();
    StellarAssetClient::new(&env, &token).mint(&merchant, &FLOAT);

    let contract_id = env.register(RefundVault, ());
    let client = RefundVaultClient::new(&env, &contract_id);
    client.initialize(&merchant, &token, &window);

    (env, client, merchant, token)
}

fn payment_ref(env: &Env, slot: u8) -> BytesN<32> {
    BytesN::from_array(env, &[slot; 32])
}

/// The on-chain challenge for a payment: `sha256(payment_ref)` zero-extended
/// to the low 32 bytes of a 128-byte big-endian value (mirrors `claim_single`).
fn challenge_for(env: &Env, payment_ref: &BytesN<32>) -> [u8; 128] {
    let hash = env
        .crypto()
        .sha256(&Bytes::from_slice(env, &payment_ref.to_array()));
    let mut challenge = [0u8; 128];
    challenge[96..].copy_from_slice(&hash.to_array());
    challenge
}

/// Honest VDF evaluation: `(y, pi) = (x^(2^t) mod N, x^(floor(2^t / l)) mod N)`
/// with `l = derive_challenge(x, y, t)`, computed by genuinely performing `t`
/// sequential squarings (small `t` in tests — the real prover would use a
/// large delay). Shares `derive_challenge` with the contract, so the
/// transcript binding is identical by construction.
fn eval_vdf(env: &Env, challenge: &[u8; 128], t: u32) -> ([u8; 128], [u8; 128]) {
    let n = U1024::from_be_slice(&vdf::MODULUS);
    let x = U1024::from_be_slice(challenge).rem(&NonZero::new(n).unwrap());

    let params = DynResidueParams::new(&n);
    let mut acc = DynResidue::new(&x, params);
    for _ in 0..t {
        acc = acc.square();
    }
    let y = acc.retrieve();

    let ell = vdf::derive_challenge(env, challenge, &y.to_be_bytes(), t);
    let mut ell_buf = [0u8; 128];
    ell_buf[112..].copy_from_slice(&ell.to_be_bytes());
    let q = U1024::ONE
        .shl(t as usize)
        .div_rem(&NonZero::new(U1024::from_be_slice(&ell_buf)).unwrap())
        .0;
    let pi = DynResidue::new(&x, params).pow(&q).retrieve();

    (y.to_be_bytes(), pi.to_be_bytes())
}

/// Packs `(output, witness)` into the 256-byte `output || witness` value the
/// contract expects on claim paths.
fn pack(env: &Env, output: &[u8; 128], witness: &[u8; 128]) -> BytesN<256> {
    let mut buf = [0u8; 256];
    buf[..128].copy_from_slice(output);
    buf[128..].copy_from_slice(witness);
    BytesN::from_array(env, &buf)
}

/// Propose + execute a policy with the given VDF delay, fast-forwarding past
/// the timelock so the policy is live. The ledger window is `0` (no expiry)
/// so the only time-like gate on claims is the VDF delay itself — the window
/// arithmetic is covered exhaustively elsewhere.
fn apply_vdf_policy(
    env: &Env,
    client: &RefundVaultClient<'static>,
    merchant: &Address,
    vdf_delay: u32,
) {
    client.deposit(merchant, &500_000);
    client.propose_policy(&0, &0, &vdf_delay);
    env.ledger().with_mut(|li| li.sequence_number += 17_280);
    client.execute_policy();
}

// ── Verifier unit tests ───────────────────────────────────────────────────

#[test]
fn test_verify_vdf_accepts_correct_proofs() {
    let env = test_env();
    for t in [16u32, 32, 64, 300] {
        for slot in [1u8, 2, 7] {
            let challenge = challenge_for(&env, &payment_ref(&env, slot));
            let (output, witness) = eval_vdf(&env, &challenge, t);
            assert_eq!(
                vdf::verify_vdf(&env, &challenge, t, &output, &witness),
                Ok(()),
                "valid proof for t={t}, slot={slot} rejected"
            );
        }
    }
}

#[test]
fn test_verify_vdf_rejects_tampered_output() {
    let env = test_env();
    let challenge = challenge_for(&env, &payment_ref(&env, 1));
    let (mut output, witness) = eval_vdf(&env, &challenge, 64);

    // Flip the low byte of the claimed output.
    output[127] ^= 0x01;
    assert_eq!(
        vdf::verify_vdf(&env, &challenge, 64, &output, &witness),
        Err(Error::InvalidVdfProof)
    );

    // An output computed for a *different* challenge must also fail.
    let other = challenge_for(&env, &payment_ref(&env, 2));
    let (output2, _) = eval_vdf(&env, &other, 64);
    assert_eq!(
        vdf::verify_vdf(&env, &challenge, 64, &output2, &witness),
        Err(Error::InvalidVdfProof)
    );
}

#[test]
fn test_verify_vdf_rejects_tampered_witness() {
    let env = test_env();
    let challenge = challenge_for(&env, &payment_ref(&env, 3));
    let (output, mut witness) = eval_vdf(&env, &challenge, 300);

    witness[127] ^= 0x01;
    assert_eq!(
        vdf::verify_vdf(&env, &challenge, 300, &output, &witness),
        Err(Error::InvalidVdfProof)
    );
}

#[test]
fn test_verify_vdf_rejects_premature_proof() {
    let env = test_env();
    let challenge = challenge_for(&env, &payment_ref(&env, 4));

    // A proof computed for t-1 squarings is not valid for t: the transcript
    // hash (and hence l and r) differs, so the check fails.
    let (output, witness) = eval_vdf(&env, &challenge, 63);
    assert_eq!(
        vdf::verify_vdf(&env, &challenge, 64, &output, &witness),
        Err(Error::InvalidVdfProof)
    );

    // Symmetrically, a proof computed for a *larger* delay must not satisfy a
    // smaller one.
    let (output2, witness2) = eval_vdf(&env, &challenge, 65);
    assert_eq!(
        vdf::verify_vdf(&env, &challenge, 64, &output2, &witness2),
        Err(Error::InvalidVdfProof)
    );
}

#[test]
fn test_verify_vdf_rejects_degenerate_challenges() {
    let env = test_env();
    let n = U1024::from_be_slice(&vdf::MODULUS);

    // x = 0, x = 1, and x = N-1 all make the delay trivially forgeable
    // (0^(2^T) = 0, 1^(2^T) = 1) — the verifier must reject them.
    let zero = [0u8; 128];
    let mut one = [0u8; 128];
    one[127] = 1;
    let n_minus_1 = n.wrapping_sub(&U1024::ONE).to_be_bytes();
    // x = N itself reduces to 0 mod N.
    let n_bytes = vdf::MODULUS;

    for bad in [&zero, &one, &n_minus_1, &n_bytes] {
        let (output, witness) = eval_vdf(&env, bad, 64);
        assert_eq!(
            vdf::verify_vdf(&env, bad, 64, &output, &witness),
            Err(Error::InvalidVdfProof),
            "degenerate challenge must be rejected"
        );
    }
}

// ── Public endpoint ───────────────────────────────────────────────────────

#[test]
fn test_verify_vdf_endpoint() {
    let (env, client, _merchant, _token) = setup(100);
    let challenge = challenge_for(&env, &payment_ref(&env, 1));
    let (output, witness) = eval_vdf(&env, &challenge, 64);

    let proof = VdfProof {
        output: BytesN::from_array(&env, &output),
        proof: BytesN::from_array(&env, &witness),
    };
    assert_eq!(
        client.try_verify_vdf(&BytesN::from_array(&env, &challenge), &64, &proof),
        Ok(Ok(()))
    );

    let bad = VdfProof {
        output: BytesN::from_array(&env, &output),
        proof: BytesN::from_array(&env, &[0u8; 128]),
    };
    assert_eq!(
        client.try_verify_vdf(&BytesN::from_array(&env, &challenge), &64, &bad),
        Err(Ok(Error::InvalidVdfProof))
    );
}

// ── Policy integration ────────────────────────────────────────────────────

#[test]
fn test_vdf_policy_requires_proof_on_refund() {
    let (env, client, merchant, _token) = setup(100);
    apply_vdf_policy(&env, &client, &merchant, 64);

    let ref_ = payment_ref(&env, 1);
    let buyer = Address::generate(&env);
    let challenge = challenge_for(&env, &ref_);
    let (output, witness) = eval_vdf(&env, &challenge, 64);

    // Without a proof the claim is rejected with VdfProofRequired.
    assert_eq!(
        client.try_refund(&ref_, &buyer, &100, &0, &100, &None),
        Err(Ok(Error::VdfProofRequired))
    );

    // With the correct proof it succeeds.
    assert_eq!(
        client.try_refund(
            &ref_,
            &buyer,
            &100,
            &0,
            &100,
            &Some(pack(&env, &output, &witness)),
        ),
        Ok(Ok(()))
    );

    // A premature proof (computed for a smaller delay) is rejected.
    let (early_output, early_witness) = eval_vdf(&env, &challenge, 63);
    assert_eq!(
        client.try_refund(
            &ref_,
            &buyer,
            &100,
            &0,
            &100,
            &Some(pack(&env, &early_output, &early_witness)),
        ),
        Err(Ok(Error::InvalidVdfProof))
    );
}

#[test]
fn test_vdf_proof_is_payment_bound() {
    let (env, client, merchant, _token) = setup(100);
    apply_vdf_policy(&env, &client, &merchant, 64);

    let ref_a = payment_ref(&env, 1);
    let ref_b = payment_ref(&env, 2);
    let buyer = Address::generate(&env);

    // A valid proof for payment A cannot be replayed against payment B: the
    // challenge is derived from the payment ref.
    let challenge_a = challenge_for(&env, &ref_a);
    let (output, witness) = eval_vdf(&env, &challenge_a, 64);

    assert_eq!(
        client.try_refund(
            &ref_b,
            &buyer,
            &100,
            &0,
            &100,
            &Some(pack(&env, &output, &witness)),
        ),
        Err(Ok(Error::InvalidVdfProof))
    );
}

#[test]
fn test_vdf_proof_without_configured_delay_fails() {
    let (env, client, merchant, _token) = setup(100);
    // Default policy: no VDF delay.
    client.deposit(&merchant, &500_000);

    let ref_ = payment_ref(&env, 1);
    let buyer = Address::generate(&env);
    let challenge = challenge_for(&env, &ref_);
    let (output, witness) = eval_vdf(&env, &challenge, 64);

    // Supplying a proof when the policy does not require one is rejected
    // rather than silently ignored.
    assert_eq!(
        client.try_refund(
            &ref_,
            &buyer,
            &100,
            &0,
            &100,
            &Some(pack(&env, &output, &witness)),
        ),
        Err(Ok(Error::VdfNotConfigured))
    );
    // And no proof is needed when no delay is configured.
    assert_eq!(
        client.try_refund(&ref_, &buyer, &100, &0, &100, &None),
        Ok(Ok(()))
    );
}

#[test]
fn test_vdf_delay_configured_and_readable() {
    let (env, client, merchant, _token) = setup(100);

    assert_eq!(client.get_vdf_delay(), 0);
    client.deposit(&merchant, &500_000);
    client.propose_policy(&100, &0, &777);

    // The pending proposal carries the delay before it is live...
    let proposal = client.get_pending_policy().unwrap();
    assert_eq!(proposal.vdf_delay, 777);
    assert_eq!(client.get_vdf_delay(), 0);

    env.ledger().with_mut(|li| li.sequence_number += 17_280);
    client.execute_policy();

    assert_eq!(client.get_vdf_delay(), 777);
    assert_eq!(client.get_pending_policy(), None);
}

#[test]
fn test_claim_batch_with_vdf_proofs() {
    let (env, client, merchant, _token) = setup(100);
    apply_vdf_policy(&env, &client, &merchant, 64);

    let ref_a = payment_ref(&env, 1);
    let ref_b = payment_ref(&env, 2);
    let buyer = Address::generate(&env);
    let (o_a, w_a) = eval_vdf(&env, &challenge_for(&env, &ref_a), 64);
    let (o_b, w_b) = eval_vdf(&env, &challenge_for(&env, &ref_b), 64);

    let claims = vec![
        &env,
        RefundClaim {
            payment_ref: ref_a.clone(),
            recipient: buyer.clone(),
            amount: 100,
            paid_at_ledger: 0,
            payment_amount: 100,
            vdf_proof: Some(pack(&env, &o_a, &w_a)),
        },
        RefundClaim {
            payment_ref: ref_b.clone(),
            recipient: buyer.clone(),
            amount: 200,
            paid_at_ledger: 0,
            payment_amount: 200,
            vdf_proof: Some(pack(&env, &o_b, &w_b)),
        },
    ];
    assert_eq!(client.try_claim_batch(&claims), Ok(Ok(())));
    assert_eq!(client.get_refund(&ref_a).unwrap().amount_refunded, 100);
    assert_eq!(client.get_refund(&ref_b).unwrap().amount_refunded, 200);

    // One bad proof in the batch fails the whole (atomic) batch.
    let bad = RefundClaim {
        payment_ref: ref_a.clone(),
        recipient: buyer,
        amount: 100,
        paid_at_ledger: 0,
        payment_amount: 100,
        vdf_proof: Some(pack(&env, &o_a, &[0u8; 128])),
    };
    assert_eq!(
        client.try_claim_batch(&vec![&env, bad]),
        Err(Ok(Error::InvalidVdfProof))
    );
}

#[test]
fn test_process_batch_with_vdf_proofs() {
    let (env, client, merchant, _token) = setup(100);
    apply_vdf_policy(&env, &client, &merchant, 64);

    let ref_a = payment_ref(&env, 1);
    let ref_b = payment_ref(&env, 2);
    let buyer = Address::generate(&env);
    let (o_a, w_a) = eval_vdf(&env, &challenge_for(&env, &ref_a), 64);
    let (o_b, _) = eval_vdf(&env, &challenge_for(&env, &ref_b), 64);

    let ok = RefundParam {
        payment_ref: ref_a.clone(),
        recipient: buyer.clone(),
        amount: 100,
        paid_at_ledger: 0,
        payment_amount: 100,
        vdf_proof: Some(pack(&env, &o_a, &w_a)),
    };
    let bad = RefundParam {
        payment_ref: ref_b,
        recipient: buyer,
        amount: 100,
        paid_at_ledger: 0,
        payment_amount: 100,
        vdf_proof: Some(pack(&env, &o_b, &[0u8; 128])),
    };
    // Best-effort: the valid claim applies, the invalid one is recorded false.
    assert_eq!(
        client.try_process_batch(&vec![&env, ok.clone(), bad]),
        Ok(Ok(vec![&env, true, false]))
    );
    assert_eq!(client.get_refund(&ref_a).unwrap().amount_refunded, 100);
}

// ── Budget ────────────────────────────────────────────────────────────────

/// How much of the default CPU / memory budget a single VDF verification may
/// consume, with headroom, before the test fails. Measured empirically on
/// 2026-08-29 against the default test host budget (100M CPU instructions /
/// 40MB): the two 1024-bit modular exponentiations dominate.
const VDF_VERIFY_MAX_CPU: u64 = 8_000_000;
const VDF_VERIFY_MAX_MEM: u64 = 5_000_000;

#[test]
fn test_vdf_verify_resource_cost_budget() {
    // Measured through the contract client so the verifier runs as actual
    // WASM (the same code path a production invocation executes) rather than
    // as native test-side Rust, which the budget does not meter.
    let (env, client, _merchant, _token) = setup(100);
    let challenge = challenge_for(&env, &payment_ref(&env, 1));
    let (output, witness) = eval_vdf(&env, &challenge, 64);
    let proof = VdfProof {
        output: BytesN::from_array(&env, &output),
        proof: BytesN::from_array(&env, &witness),
    };

    env.cost_estimate().budget().reset_default();
    assert_eq!(
        client.try_verify_vdf(&BytesN::from_array(&env, &challenge), &64, &proof),
        Ok(Ok(()))
    );
    let cpu = env.cost_estimate().budget().cpu_instruction_cost();
    let mem = env.cost_estimate().budget().memory_bytes_cost();

    assert!(
        cpu <= VDF_VERIFY_MAX_CPU,
        "VDF verify CPU cost regression! Measured: {cpu}, Limit: {VDF_VERIFY_MAX_CPU}"
    );
    assert!(
        mem <= VDF_VERIFY_MAX_MEM,
        "VDF verify memory cost regression! Measured: {mem}, Limit: {VDF_VERIFY_MAX_MEM}"
    );
}
