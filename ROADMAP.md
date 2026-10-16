# Roadmap

## Issue #443: stACC liquid staking derivative minting

- [x] Implement stACC liquid staking derivative minting (#443)
  - New `liquid_staking` module in `contracts/governance/src/liquid_staking.rs`
  - 1:1 mint/burn of stACC against locked underlying tokens
  - Exchange rate progression and lock-epoch-based redemption
  - CHANGELOG.md entry added

## Documentation: rustdoc field docs for contracttype/event payloads

- [x] Document the publicly exported fields of `RefundParam` in refund-vault (#784)
- [x] Document the publicly exported fields of `YieldInfo` in refund-vault (#785)
- [x] Document the publicly exported fields of `CouponAppliedEvent` in refund-vault (#783)
- [x] Document the publicly exported fields of `HtlcAddedEvent` in state-channel (#786)