# Changelog

All notable changes to `ReceiptAnchor` and `RefundVault` are recorded here.

The two contracts are versioned together and share a tag. Versioning follows the
policy in [`docs/RELEASING.md`](docs/RELEASING.md): while the project is pre-1.0,
breaking changes bump the **minor** version, and they are called out as such.

## [Unreleased]

- **`refund-vault` (issue #474): test NFT escrow events.** Added coverage for NFT escrow deposits, merchant claims, buyer refunds, and rejected operations, verifying emitted events, escrow records, and NFT ownership transitions.
- **`state-channel` (issue #622): hashlock reserve bookkeeping pinned by
  tests.** New tests in `contracts/state-channel/src/hashlock_test.rs`
  assert, after every add/resolve/refund transition, that `HashlockReserved`
  equals the sum of the still-pending payments' amounts; that
  `HashlockPaymentCount` allocates distinct sequential ids that a resolve
  neither rewinds nor reuses; and that a reveal and a timed-out HTLC refund
  each release exactly their own amount, with a double reveal rejected and the
  reserve left at the pending sum. Test-only — no contract behavior
  change.
- security(multisig): implement 14-day TTL expiration for pending signatures and approvals (#449)
- **`nft` (issue #453): Discount coupon NFTs for merchant stores.** New coupon
  module (`contracts/refund-vault/src/coupons.rs`) implements single-use discount
  coupons that grant percentage discounts on escrow deposits. Coupons are minted
  by the vault admin, have an owner address, a discount rate in basis points
  (max 50%), and are marked redeemed on first use. The `deposit` function now
  accepts an optional `coupon_id` parameter; when provided, the discount is applied
  atomically before the token transfer. Added error codes `CouponNotFound` and
  `CouponAlreadyRedeemed` to the common Error enum.
### Added
- **`privacy` (issue #440): Groth16 verification for concealed escrow amounts.**
  New `privacy` contract (`contracts/privacy/`) holds an escrow against a
  commitment to a concealed amount and verifies a Groth16 proof whose public
  inputs carry that amount and the commitment's value/blinding.
  `create_escrow` stores the commitment and its floor, `verify_escrow` checks
  the proof structurally (`groth16::verify_groth16`: IC count, non-empty
  well-sized points, no forged zeros), decodes the proven amount from
  `public_inputs[0]` and marks the escrow `Verified` only when it is at or above
  the floor, otherwise `Rejected`; a second verification of the same escrow
  fails with `Error::AlreadyVerified`. `Error` is crate-local on purpose, so
  growing it does not enlarge the WASM spec of the contracts that share
  `accensa_common::Error`. The commitment is `sha256(value || blinding)` and
  the verifier is structural rather than pairing-based, because Soroban exposes no
  curve arithmetic — both are documented in the module as stand-ins.
- **`cross-chain` (issue #454): Axelar gateway deposit adapter.** New `axelar`
  module lets users on Ethereum, Polygon and other Axelar-chains fund Soroban
  escrow balances. `axelar_execute` runs only for the admin-registered gateway
  address, and the gateway must first confirm the inbound message through
  `AxelarGatewayInterface::validate_message`, so the contract never re-implements
  Axelar's validator-set checks. The version-1 deposit payload is validated
  structurally before decoding (exact 49 bytes: version, 32-byte bridged account
  id, big-endian i128 amount that must be positive; anything else is
  `Error::InvalidProof`, so a future version cannot silently decode as v1), and
  each `message_id` is recorded and refused on replay (`Error::AlreadyRefunded`).
- **`insurance` (issue #445): Compound-style interest accrual on the
  insurance pool.** New `accensa-insurance` contract
  (`contracts/insurance`, `src/interest.rs`) models a lending pool at 1e18
  fixed-point precision: utilization in basis points drives an algorithmic
  borrow rate (`BASE_RATE_BPS + SLOPE_BPS * utilization`) and a supply rate
  reduced by a 10% reserve factor; global borrow/supply indexes compound as
  `index * (1 + rate * dt)` and update on every interaction. Suppliers
  deposit/withdraw capital, borrowers draw against their settled supplied
  balance (`ExceedsCollateral` above it, `InsufficientLiquidity` above the
  pool float), and every call settles the user's accrued interest into their
  checkpointed balances before mutating positions. Withdrawals are rejected
  when they would leave `borrowed > supplied - amount`, preventing
  collateral withdrawal that leaves debt unbacked. Includes 16 tests
  covering accrual, liquidity, collateral, settlement, and the withdrawal
  safeguard.
- **`receipt-shard` (issue #437): shard storage consolidation.** Router-authorized source shards can migrate exact `BatchRecord` values into a destination shard, verify the returned record before [...]
- **`state-channel` (issue #431): anti-sniping late counter-proof extension.** A
  valid counter-proof submitted within the final 50 ledgers of the dispute window
  now pushes the deadline back by 50 ledgers so the honest party has time to
  answer, instead of letting a hostile party front-run settlement in the last
  block. Extensions are capped at 3; a fourth late counter-proof is rejected with
  the new shared `Error::DisputeExtensionLimitReached` (code 417, appended so no
  existing error code moves).
- **`oracle`: Chainlink data-feed consumer trait.** New `accensa-oracle` contract (`contracts/oracle/src/chainlink.rs`) implements an AggregatorV3-style consumer: admin-pushed `RoundData` with rou[...]
