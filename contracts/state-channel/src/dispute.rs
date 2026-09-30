//! Dispute-window timing helpers for the state channel contract (issue #387).
//!
//! A cooperative close starts a challenge window (`closed_at +
//! challenge_period`) during which the receiver may file a dispute. Filing
//! the dispute transitions the channel into [`ChannelPhase::Disputed`] and
//! re-arms the window from the ledger the dispute was initiated
//! (`disputed_at + challenge_period`). While that window is open, anyone
//! holding a sender-signed, newer state may submit **counter-evidence**
//! (each accepted state re-arms the window). Once the window elapses with no
//! counter-evidence, the channel is settled strictly per the **last verified
//! state** via `finalize_dispute`, paid to anyone.
//!
//! These helpers centralise the two boundary checks — "window still open"
//! and "window has elapsed" — used by `dispute`, `submit_counter_evidence`
//! and `finalize_dispute` in the contract implementation, so the timing
//! rules stay consistent and unit-testable.

use crate::Error;
use soroban_sdk::Env;

/// Ledgers before the deadline at which a counter-proof counts as "late" and
/// extends the dispute window (issue #431).
pub(crate) const COUNTER_PROOF_EXTENSION_LEDGERS: u32 = 50;

/// Maximum number of times the dispute window may be extended by late
/// counter-proofs (issue #431). Bounds the window so a hostile party cannot
/// stall settlement indefinitely by resubmitting newer states.
pub(crate) const MAX_DISPUTE_EXTENSIONS: u32 = 3;

/// Apply the anti-sniping late-counter-proof rule (issue #431).
///
/// When a valid counter-proof is submitted within the final
/// [`COUNTER_PROOF_EXTENSION_LEDGERS`] ledgers of the window, the window is
/// extended by that many ledgers so the honest party has time to respond.
/// Extensions are capped at [`MAX_DISPUTE_EXTENSIONS`]; a counter-proof that
/// would exceed the cap is rejected with
/// [`Error::DisputeExtensionLimitReached`].
///
/// Returns the (possibly unchanged) `(window_started, extensions)`.
pub(crate) fn extend_window_on_late_counter_proof(
    env: &Env,
    window_started: u32,
    challenge_period: u32,
    extensions: u32,
) -> Result<(u32, u32), Error> {
    let deadline = window_started.saturating_add(challenge_period);
    let current_ledger = env.ledger().sequence();
    // Outside the final stretch, a counter-proof does not extend the window.
    if current_ledger > deadline
        || deadline.saturating_sub(current_ledger) >= COUNTER_PROOF_EXTENSION_LEDGERS
    {
        return Ok((window_started, extensions));
    }
    if extensions >= MAX_DISPUTE_EXTENSIONS {
        return Err(Error::DisputeExtensionLimitReached);
    }
    Ok((
        window_started.saturating_add(COUNTER_PROOF_EXTENSION_LEDGERS),
        extensions + 1,
    ))
}

/// The dispute window is still open at `current_ledger`, i.e. the lapsed
/// deadline (`window_started + challenge_period`) has not yet passed.
///
/// Returns `Err(Error::ChallengeActive)` when the window is closed, since a
/// caller attempting an in-window action (dispute / counter-evidence) is too
/// late.
pub(crate) fn ensure_window_open(
    env: &Env,
    window_started: u32,
    challenge_period: u32,
) -> Result<(), Error> {
    let current_ledger = env.ledger().sequence();
    let deadline = window_started.saturating_add(challenge_period);
    if current_ledger > deadline {
        return Err(Error::ChallengeExpired);
    }
    Ok(())
}

/// The dispute window has fully elapsed at `current_ledger`, i.e. the
/// deadline (`window_started + challenge_period`) is strictly in the past,
/// so `finalize_dispute` may now settle.
///
/// Returns `Err(Error::ChallengeActive)` while the window is still open,
/// since finalising early would let a stale state win.
pub(crate) fn ensure_window_elapsed(
    env: &Env,
    window_started: u32,
    challenge_period: u32,
) -> Result<(), Error> {
    let current_ledger = env.ledger().sequence();
    let deadline = window_started.saturating_add(challenge_period);
    if current_ledger <= deadline {
        return Err(Error::ChallengeActive);
    }
    Ok(())
}
