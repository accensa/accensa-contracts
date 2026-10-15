# Roadmap

## Issue #443: stACC liquid staking derivative minting

- [x] Implement stACC liquid staking derivative minting (#443)
  - New `liquid_staking` module in `contracts/governance/src/liquid_staking.rs`
  - 1:1 mint/burn of stACC against locked underlying tokens
  - Exchange rate progression and lock-epoch-based redemption
  - CHANGELOG.md entry added

## Issue #660: PR template verification step runs the full workspace suite

- [x] Point the PR template's Verification checkbox at `cargo test --workspace` (#660)

## Issue #658: CHANGELOG's pre-1.0 versioning policy matches RELEASING.md

- [x] State the pre-1.0 minor-for-breaking rule once in `docs/RELEASING.md` and cite it from `CHANGELOG.md` (#658)