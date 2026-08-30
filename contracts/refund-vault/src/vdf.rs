//! Verifiable Delay Function (VDF) verification (Wesolowski scheme, issue #138).
//!
//! A VDF is a function `f(x, T) = x^(2^T) mod N` that takes a fixed number of
//! *sequential* squarings to evaluate but whose output can be verified in a
//! handful of modular exponentiations. The sequential cost is what makes the
//! delay real: nobody — including a validator that controls block timestamps
//! or transaction ordering — can produce the output faster than `T` squarings,
//! unless they know the factorization of `N`.
//!
//! This module implements the **verifier** side only. The prover (typically the
//! merchant's refund agent) computes the delay off-chain and submits a
//! [`VdfProof`]; the contract checks it in microseconds, so the whole scheme
//! fits Soroban's WASM computational budget (pinned by
//! `test_vdf_verify_resource_cost_budget` in `vdf_test.rs`).
//!
//! # Scheme
//!
//! Given a challenge `x`, a delay parameter `T` (number of squarings), a claimed
//! output `y = x^(2^T) mod N`, and a proof `pi = x^(floor(2^T / l)) mod N`
//! where `l` is a prime challenge derived from the transcript, verification is:
//!
//! 1. Derive `l = next_prime(SHA-256(x || y || T))` — a fresh 128-bit prime
//!    bound to the transcript, so a proof cannot be replayed across inputs and
//!    the prover cannot choose `l` after seeing `y` (Fiat-Shamir style, as in
//!    Chia's VDF).
//! 2. Compute `r = 2^T mod l`.
//! 3. Accept iff `y == pi^l * x^r (mod N)`.
//!
//! Soundness: writing `2^T = q*l + r`, an honest prover's `pi = x^q` satisfies
//! `pi^l * x^r = x^(q*l + r) = x^(2^T) = y`. A cheating prover must find a
//! `pi` with `pi^l = x^(q*l)` — an `l`-th root extraction modulo the composite
//! `N`, which is believed infeasible without the factorization (the *adaptive
//! root assumption*). The verification cost is independent of `T`: only `l`
//! and `r` (both 128-bit) are ever used as exponents on-chain.
//!
//! # Modulus
//!
//! `MODULUS` is a fixed 1024-bit RSA modulus baked into the contract. Its
//! factorization is deliberately *not* published — anyone who factors `N` can
//! compute `x^(2^T) mod N` in `O(log T)` steps via `phi(N)` and break the
//! delay. The constant below was generated for this release with its prime
//! factors discarded after generation; because both contracts are immutable,
//! rotating the modulus requires a new deployment (the repo's standard
//! migration path, see `docs/ADR-003-upgradeability.md`). A production
//! deployment should replace it with a modulus from a trusted-setup ceremony
//! (see `docs/SECURITY_MODEL.md` § "VDF Fairness").
//!
//! # Challenge domain
//!
//! The challenge `x` must satisfy `1 < x mod N < N - 1`. The `x = 0` / `x = 1`
//! cases are degenerate (`0^(2^T) = 0`, `1^(2^T) = 1` for every `T`) and would
//! otherwise let a caller "prove" any delay with no work at all.

use accensa_common::Error;
use crypto_bigint::{
    modular::runtime_mod::{DynResidue, DynResidueParams},
    Encoding, NonZero, U1024, U128,
};
use soroban_sdk::{contracttype, Bytes, BytesN, Env};

/// Fixed 1024-bit RSA modulus for the delay group.
///
/// Generated as the product of two 512-bit primes for this release; the prime
/// factors were discarded immediately after generation and are not stored
/// anywhere in this repository. See the module docs above for the security
/// rationale and the ceremony guidance in `docs/SECURITY_MODEL.md`.
pub const MODULUS: [u8; 128] = [
    0xb5, 0x6f, 0xda, 0xb2, 0xf4, 0xee, 0x59, 0x57, 0xcd, 0x1b, 0x66, 0x38, 0xb3, 0x6b, 0xd2, 0x72,
    0x70, 0x7a, 0xde, 0xbe, 0x94, 0x84, 0x99, 0x6f, 0xdd, 0x9b, 0xe6, 0xce, 0xdb, 0x8d, 0xbf, 0x99,
    0xe1, 0x18, 0xe6, 0x79, 0xb2, 0x74, 0xb6, 0x35, 0xc6, 0x5e, 0xd3, 0x58, 0xfd, 0x8a, 0x66, 0xd1,
    0xb5, 0x6c, 0xce, 0x5f, 0x96, 0xdb, 0x59, 0x2e, 0x3c, 0xae, 0xab, 0xc3, 0x6c, 0xf9, 0xfd, 0xac,
    0x5b, 0x73, 0x57, 0x99, 0x4e, 0xfe, 0xfb, 0x6e, 0x03, 0x13, 0x65, 0x32, 0x4d, 0x1b, 0x20, 0xb3,
    0xb3, 0xc3, 0xf3, 0x4d, 0x59, 0xf8, 0x46, 0xcd, 0xaf, 0xed, 0x1f, 0x92, 0x1e, 0x05, 0x20, 0xaf,
    0xf3, 0x42, 0xd8, 0xcb, 0x18, 0xa8, 0x94, 0x23, 0x6c, 0x9a, 0x8e, 0x6f, 0x15, 0xdf, 0xcc, 0xc0,
    0xe7, 0x48, 0x6d, 0x57, 0x9f, 0x79, 0xc0, 0xfa, 0x1a, 0xcd, 0x56, 0xdd, 0x2a, 0x09, 0xc7, 0xf5,
];

/// A Wesolowski VDF proof: the claimed output `y = x^(2^T) mod N` and the
/// witness `pi = x^(floor(2^T / l)) mod N` for the transcript-derived prime
/// `l`. Both are 1024-bit values (the modulus width), big-endian.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VdfProof {
    /// The claimed VDF output, `x^(2^T) mod N`.
    pub output: BytesN<128>,
    /// The Wesolowski witness, `x^(floor(2^T / l)) mod N`.
    pub proof: BytesN<128>,
}

/// Deterministic Miller-Rabin witness bases for 128-bit candidates.
///
/// There is no *proven* small fixed set for all of `2^128`, but the first
/// sixteen primes are a strong-pseudoprime test with astronomically small
/// error for 128-bit inputs (no strong pseudoprime to the first twelve bases
/// is known below `3.3e24`, let alone `2^128`), and the candidate here is
/// drawn from SHA-256, so an adversary cannot search for a failing input
/// without breaking preimage resistance. This is the pragmatic standard used
/// by on-chain primality checks.
const MR_BASES: [u32; 16] = [2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53];

/// Verifies a Wesolowski proof that `output == challenge^(2^delay) mod N`.
///
/// `challenge`, `output` and `proof` are raw big-endian 1024-bit values. The
/// challenge is reduced mod `N` and must land in `(1, N-1)`; `output` and
/// `proof` are reduced mod `N`. The challenge prime `l` is derived from the
/// *raw* transcript bytes so the prover can reproduce it exactly.
///
/// Returns `Ok(())` iff the equation holds; `Error::InvalidVdfProof`
/// otherwise. Pure and read-only — no storage, no auth, no state change.
pub(crate) fn verify_vdf(
    env: &Env,
    challenge: &[u8; 128],
    delay: u32,
    output: &[u8; 128],
    proof: &[u8; 128],
) -> Result<(), Error> {
    let n = U1024::from_be_slice(&MODULUS);

    // Reduce the challenge and reject the degenerate cases x ≡ 0, 1, or N-1.
    let x = U1024::from_be_slice(challenge).rem(&NonZero::new(n).unwrap());
    if x <= U1024::ONE || x >= n.wrapping_sub(&U1024::ONE) {
        return Err(Error::InvalidVdfProof);
    }
    let y = U1024::from_be_slice(output).rem(&NonZero::new(n).unwrap());
    let pi = U1024::from_be_slice(proof).rem(&NonZero::new(n).unwrap());

    // Challenge prime bound to the transcript (x, y, T), as passed in.
    let ell = derive_challenge(env, challenge, output, delay);

    // r = 2^delay mod ell, then check y == pi^ell * x^r (mod N).
    let r = pow_mod_128(&U128::from_u32(2), &ell, &U128::from_u32(delay));

    let params = DynResidueParams::new(&n);
    let pi_pow = DynResidue::new(&pi, params).pow(&u128_to_u1024(&ell));
    let x_pow = DynResidue::new(&x, params).pow(&u128_to_u1024(&r));
    let rhs = pi_pow.mul(&x_pow).retrieve();

    if rhs == y {
        Ok(())
    } else {
        Err(Error::InvalidVdfProof)
    }
}

/// Derives the per-proof challenge prime `l` from the transcript:
/// `l = next_prime(first 16 bytes of SHA-256(x || y || T))`.
///
/// Binding `l` to `(x, y, T)` via a hash is the Fiat-Shamir transformation of
/// the interactive Wesolowski protocol: the prover must commit to `y` before
/// `l` exists, and a forged `y` would have to survive a random `l` chosen
/// after the fact.
pub(crate) fn derive_challenge(
    env: &Env,
    challenge: &[u8; 128],
    output: &[u8; 128],
    delay: u32,
) -> U128 {
    let mut buf = [0u8; 260];
    buf[..128].copy_from_slice(challenge);
    buf[128..256].copy_from_slice(output);
    buf[256..260].copy_from_slice(&delay.to_be_bytes());
    let digest = env
        .crypto()
        .sha256(&Bytes::from_slice(env, &buf))
        .to_array();

    let mut candidate = U128::from_be_slice(&digest[..16]);
    // l must be odd for Miller-Rabin; step by 2 to the next prime.
    if !candidate.bit_vartime(0) {
        candidate = candidate.wrapping_add(&U128::ONE);
    }
    while !miller_rabin(&candidate) {
        candidate = candidate.wrapping_add(&U128::from_u32(2));
    }
    candidate
}

/// `base^exp mod m` for 128-bit values, via Montgomery arithmetic.
fn pow_mod_128(base: &U128, modulus: &U128, exp: &U128) -> U128 {
    let params = DynResidueParams::new(modulus);
    DynResidue::new(base, params).pow(exp).retrieve()
}

/// Deterministic Miller-Rabin primality test for 128-bit candidates.
///
/// Decomposes `n - 1 = d * 2^s` with `d` odd and checks every base in
/// [`MR_BASES`]. Returns `true` for the two smallest primes directly.
fn miller_rabin(n: &U128) -> bool {
    if *n <= U128::from_u32(3) {
        return *n >= U128::from_u32(2);
    }

    let one = U128::ONE;
    let n_minus_1 = n.wrapping_sub(&one);

    let mut d = n_minus_1;
    let mut s = 0u32;
    while !d.bit_vartime(0) {
        d = d.shr(1);
        s += 1;
    }

    let params = DynResidueParams::new(n);
    'base: for base in MR_BASES {
        let a = U128::from_u32(base);
        if a >= *n {
            continue;
        }
        let mut x = DynResidue::new(&a, params).pow(&d).retrieve();
        if x == one || x == n_minus_1 {
            continue;
        }
        let mut i = 1;
        while i < s {
            x = DynResidue::new(&x, params).square().retrieve();
            if x == n_minus_1 {
                continue 'base;
            }
            i += 1;
        }
        return false;
    }
    true
}

/// Widens a 128-bit value to 1024 bits (big-endian zero-extension).
fn u128_to_u1024(v: &U128) -> U1024 {
    let mut buf = [0u8; 128];
    buf[112..].copy_from_slice(&v.to_be_bytes());
    U1024::from_be_slice(&buf)
}
