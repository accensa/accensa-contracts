# Upstream `upto` Specification Research Notes

> **Purpose:** Detailed research notes from reading the upstream x402 `upto`
> specifications. These notes support ADR-002 §6.1 and answer the five research
> questions listed there. This document is a reference; the ADR contains the
> concise conclusion.

## Source Record

- **Upstream repository:** `x402-foundation/x402`
- **Exact commit SHA:** `b32b5640557ff793c3ecbfac6f933b0ad3b2170b`
- **Date of research:** 2026-08-26
- **Files/specifications inspected:**
  - `specs/schemes/upto/scheme_upto.md` — chain-agnostic upto spec
  - `specs/schemes/upto/scheme_upto_evm.md` — EVM upto implementation spec
  - `specs/schemes/upto/scheme_upto_svm.md` — SVM/Solana upto implementation spec
  - `specs/x402-specification-v2.md` — core x402 v2 protocol specification
  - `specs/schemes/exact/scheme_exact_stellar.md` — existing Stellar exact spec (for reference patterns)
  - `specs/schemes/exact/scheme_exact.md` — exact scheme chain-agnostic spec (for flow reference)
  - `specs/schemes/auth-capture/scheme_auth_capture.md` — auth-capture scheme (for lifecycle reference)
  - `specs/scheme_template.md` — scheme template
  - `specs/scheme_impl_template.md` — scheme implementation template
  - `specs/README.md` — specs overview

**Note on the SVM specification:** The upstream repository at this commit contains a
finalized `scheme_upto_svm.md`. It is NOT a draft or RFC — it is a complete
implementation-specific specification at the same maturity level as `scheme_upto_evm.md`.

---

## Research Question 1: Wire Format

### What the upto flow carries

#### PaymentPayload structure (core spec §5.2)

The `PaymentPayload` is the outer envelope. From the core x402 spec:

```json
{
  "x402Version": 2,
  "resource": { /* ResourceInfo */ },
  "accepted": { /* PaymentRequirements */ },
  "payload": { /* scheme-specific */ },
  "extensions": {}
}
```

The `accepted` field is a `PaymentRequirements` object. The `payload` field is
scheme-specific.

#### EVM upto PaymentPayload payload

From `scheme_upto_evm.md`, the `payload` field contains:

| Field | Description |
|---|---|
| `signature` | EIP-712 signature for `permitWitnessTransferFrom` |
| `permit2Authorization.permitted.token` | ERC-20 token address |
| `permit2Authorization.permitted.amount` | **Maximum** authorized amount (the ceiling) |
| `permit2Authorization.from` | Payer wallet address |
| `permit2Authorization.spender` | Permit2 proxy contract address |
| `permit2Authorization.nonce` | Unique nonce for replay protection |
| `permit2Authorization.deadline` | Expiry timestamp |
| `permit2Authorization.witness.to` | **Recipient** (bound at sign time) |
| `permit2Authorization.witness.facilitator` | Bound facilitator address |
| `permit2Authorization.witness.validAfter` | Start time |

#### SVM upto PaymentPayload payload (`UptoPayload`)

From `scheme_upto_svm.md`, the `payload` field contains:

| Field | Description |
|---|---|
| `from` | Payer wallet |
| `maxAmount` | Signed ceiling (must equal verification-phase `amount`) |
| `expiresAt` | Deadline (Unix seconds), signed into server voucher |
| `validAfter` | Activation time (Unix seconds) |
| `nonce` | Unique salt for channel PDA derivation |
| `openSlot` | Slot for channel PDA seed |
| `channelId` | Channel PDA (derived before signing) |
| `deposit` | On-chain escrow amount (must equal `maxAmount`) |
| `authorizedSigner` | Must equal `extra.receiverAuthorizer` |
| `openTransaction` | Base64 partially-signed `open` transaction |
| `voucherSignature` | *(settlement-time only)* Server's Ed25519 voucher |

**Critical:** The `voucherSignature` is NOT part of the client's `PAYMENT-SIGNATURE`
payload. The client signs only `open`. After metering, the server signs an Ed25519
voucher and attaches it to the settle-time `paymentPayload.payload`. From the SVM spec:

> "The voucher is not carried in the client's `PAYMENT-SIGNATURE` payload — the
> client signs only `open`. After metering, the server signs an Ed25519 voucher
> with `receiverAuthorizer` and transmits it to the facilitator in the settlement
> request (`payload.voucherSignature`)."

#### PaymentRequirements fields

From the chain-agnostic `scheme_upto.md`:

| Field | Type | Description |
|---|---|---|
| `scheme` | string | `"upto"` |
| `network` | string | CAIP-2 network identifier |
| `amount` | string | **Phase-dependent:** maximum at verify, actual at settle |
| `asset` | string | Token address |
| `payTo` | string | Recipient address |
| `maxTimeoutSeconds` | number | Completion window |
| `extra` | object | Scheme/network-specific additional info |

#### Phase-dependent amount semantics (MUST-level requirement)

From `scheme_upto.md` §Phase-Dependent `amount` Semantics:

> "At **verification** time, `amount` represents the **maximum** amount the client
> authorizes. At **settlement** time, `amount` represents the **actual amount to
> settle**, which MUST be less than or equal to the previously authorized maximum."
>
> "The actual settled amount is communicated by the resource server to the facilitator
> via the `amount` field in the settlement-time `PaymentRequirements`."

From the core spec §7.2:

> "While the request structure is identical, some payment schemes may assign different
> semantics to fields at settlement time versus verification time. For example, in the
> `upto` scheme, the `amount` field in `paymentRequirements` represents the maximum
> authorized amount at verification time, but the actual amount to settle at settlement
> time."

#### What the facilitator receives during verify

The facilitator receives the `PaymentPayload` (containing the client's signed
authorization with the ceiling) and `PaymentRequirements` (where `amount` is the
maximum). From the EVM spec §Phase 3:

1. Verify signature is valid and recovers to `permit2Authorization.from`.
2. Verify client has Permit2 approval.
3. Verify client has sufficient balance for `amount`.
4. **Verify `permit2Authorization.permitted.amount` equals `requirements.amount`.**
   *(This equality check applies at verification time only, where both carry the
   ceiling.)*
5. Verify deadline and validAfter.
6. Verify token and network match.
7. Simulate settlement with full `amount` (worst case).

#### What the facilitator receives during settle

The facilitator receives the same `PaymentPayload` structure, but `PaymentRequirements
.amount` now carries the **actual settlement amount** (set by the resource server).

From the EVM spec §Settle-Time Verification:

> "Before executing an on-chain settlement, the facilitator MUST re-verify the client's
> signature. Because the `upto` scheme uses phase-dependent `amount` semantics, the
> `/settle` request will carry `paymentRequirements.amount` set to the **actual settlement
> amount**... which may be less than `paymentPayload.payload.permit2Authorization
> .permitted.amount`."

Settlement steps:

1. **Verify the signature against `permitted.amount`** — NOT against
   `paymentRequirements.amount`.
2. **Validate `paymentRequirements.amount <= permit2Authorization.permitted.amount`.**
3. **Execute the on-chain transfer for `paymentRequirements.amount`.**

From the EVM spec:

> "**Conformance note**: A facilitator that enforces
> `paymentRequirements.amount === permit2Authorization.permitted.amount` at settle time
> will reject all partial settlements, breaking the core `upto` value proposition."

#### How the actual amount is represented

The actual amount is NOT a separate payload field. It is the `PaymentRequirements.amount`
field at settlement time, supplied by the **resource server**. From `scheme_upto.md`:

> "The actual settled amount is communicated by the resource server to the facilitator
> via the `amount` field in the settlement-time `PaymentRequirements`. This allows the
> resource server to determine the final charge based on actual resource consumption
> (e.g., tokens generated, bytes transferred) and communicate it to the facilitator
> without requiring additional fields or a separate settlement type."

On EVM, the facilitator then calls `x402Permit2Proxy.settle` with this actual amount.
On SVM, the server signs a voucher for the actual amount and the facilitator builds
`settle_and_seal` + `distribute`.

#### SettlementResponse

From `scheme_upto_evm.md` §3:

| Field | Type | Required | Description |
|---|---|---|---|
| `success` | boolean | yes | Whether settlement succeeded |
| `errorReason` | string | no | Error if failed |
| `payer` | string | no | Payer wallet address |
| `transaction` | string | yes | Blockchain tx hash (empty string if $0) |
| `network` | string | yes | CAIP-2 network |
| `amount` | string | yes | **Actual** amount charged (may be 0) |

---

## Research Question 2: Invocation Count

### Whether EVM settlement is one on-chain call

**Yes.** On EVM, settlement is a single on-chain call:
`x402Permit2Proxy.settle(permit, actualAmount, owner, witness, signature)`.
The Permit2 `permitWitnessTransferFrom` does everything in one call: validates the
signature, checks the nonce, and transfers tokens.

### Whether SVM settlement is one or multiple on-chain transactions

**Multiple instructions in a settlement sequence.** From `scheme_upto_svm.md`:

> "Settlement happens after the resource server executes the metered work and before
> it returns the response to the client. The overall order is
> `settle(deposit)` → resource execution → `settle(claim)` → serve."

The SVM spec uses the **`escrow` payment flow** (x402 v2 §6.1):

| Flow | Ordering |
|---|---|
| `escrow` | settle → resource → settle → respond |

The settlement-side instructions are: `settle_and_seal` (optionally with Ed25519
voucher) then `distribute`. These are typically bundled in one Solana transaction, but
the protocol-level flow involves **two `/settle` HTTP calls** (deposit and claim).

### Whether the spec REQUIRES one on-chain invocation

**No.** The upstream specification does NOT require exactly one on-chain invocation or
one `/settle` call. From the core spec §7.2:

> "`/settle` MAY be invoked more than once for a single payment (for example, the
> `escrow` flow settles a deposit before the resource executes and the final charge
> after). A scheme defining multiple settles MUST specify how the facilitator
> distinguishes them from payload content."

### The distinction: protocol-level vs. implementation-specific

**Protocol-level MUST requirements** (from `scheme_upto.md`):

1. **Single-Use Authorization:** "Each authorization MUST be settled at most once."
2. **Time-Bound Authorization:** MUST have `validAfter` and `deadline`.
3. **Recipient Binding:** MUST cryptographically bind the recipient address.
4. **Maximum Amount Enforcement:** Settled amount MUST be `<=` authorized maximum.
5. **Phase-dependent `amount` semantics.**

**NOT a protocol-level requirement:**

- Exactly one `/settle` HTTP call.
- Exactly one on-chain transaction.
- Any particular transaction structure.

The "single-use" requirement constrains the **authorization** (it can be settled at
most once), not the number of HTTP settle calls or on-chain instructions used to
achieve that settlement.

### Evaluation of Stellar two-invocation construction

The proposed Stellar construction in ADR-002 §4 uses:

```
authorize(payment_id, from, to, cap, expiry)  →  settle(payment_id, actual)
```

This maps directly to the `escrow` flow:

| Escrow step | Stellar equivalent |
|---|---|
| First `settle(deposit)` | `authorize()` — commits ceiling, recipient, and creates on-chain binding |
| Resource execution | Metering happens |
| Second `settle(claim)` | `settle(actual)` — transfers actual amount, sets consumed flag |

**The two-invocation construction is VIABLE.** It is structurally analogous to the SVM
`upto` escrow flow. The upstream spec explicitly permits multiple settle calls (core
spec §7.2) and the SVM `upto` spec explicitly uses the `escrow` flow with two settle
calls.

**What still needs Stellar-specific design work:**

1. **Auth entry expiration:** Soroban `signatureExpirationLedger` is short (~12
   ledgers, ~60 seconds). The `authorize` call's auth entry must cover the time needed
   for metering + settle. If metering takes longer than the auth entry lifetime, a
   different auth mechanism is needed.
2. **Single-transaction vs. two-transaction:** If both invocations happen in one
   transaction (as ADR-002 §4 suggests), the auth entry expiration is not a problem.
   If they are separate transactions, the auth entry must survive until settlement.
3. **Protocol flow naming:** The Stellar spec should declare `extra.paymentFlow:
   "escrow"` to match the SVM precedent, rather than defaulting to `authorization`.
4. **Distinguishing deposit vs. claim settles:** The protocol requires the facilitator
   to distinguish the two settles. On SVM this is done from payload content (voucher
   present → claim; no voucher → deposit). Stellar needs an equivalent mechanism.
5. **Authorization record state management:** The `authorize` call creates on-chain
   state that `settle` later reads. The lifecycle, TTL, and cleanup of this state must
   be designed.

---

## Research Question 3: CAP

### Maximum authorized amount representation

- **EVM:** `permit2Authorization.permitted.amount` in the client's signed payload.
  The Permit2 contract enforces this on-chain.
- **SVM:** `deposit` escrowed on-chain in the payment channel. The verifier pins
  `deposit == maxAmount`. From the SVM spec: "Onchain `deposit` is the ceiling and
  vouchers must satisfy `settled < cumulative_amount <= deposit`; the verifier pins
  `deposit == maxAmount` so the x402 ceiling is exact, not advisory."
- **Stellar (proposed):** Would be the `cap` argument to `authorize()`, stored in the
  authorization record on-chain.

### What value the client signs

- **EVM:** The client signs `permit2Authorization` which includes `permitted.amount`
  (the ceiling). The signature commits to this value.
- **SVM:** The client signs the `open` transaction, which commits to `deposit` (the
  ceiling) via the `open` instruction. The `open` instruction MUST encode
  `deposit == payload.maxAmount`.
- **Stellar (proposed):** The client signs an auth entry that commits to the contract
  invocation arguments, which would include `cap`.

### How settlement is constrained to ≤ maximum

From the chain-agnostic spec:

> "The settled `amount` MUST be `<=` the authorized maximum"

- **EVM facilitator check:** `paymentRequirements.amount <=
  permit2Authorization.permitted.amount` (checked at settle time).
- **SVM program check:** `settled < cumulative_amount <= deposit` enforced on-chain.
  Plus facilitator off-chain check: `paymentRequirements.amount <= maxAmount`.
- **Stellar (proposed):** The `settle` call would assert `actual <= cap` by reading
  the authorization record.

### Recipient binding

From `scheme_upto.md`:

> "The authorization MUST cryptographically bind the recipient address. The
> server/facilitator cannot redirect funds to a different address than what the client
> signed."

- **EVM:** `permit2Authorization.witness.to` is bound in the EIP-712 signature. The
  `x402Permit2Proxy` enforces that the transfer goes to `witness.to`.
- **SVM:** The distribution `[{ recipient: payTo, bps: 10000 }]` is committed at
  `open` via `distribution_hash`. The program re-checks `distribution_hash` at
  `distribute`. From the SVM spec: "The distribution fixed at `open` sends settled
  funds to `payTo`, and the program re-checks `distribution_hash` at `distribute`."
- **Stellar (proposed):** The `to` address is recorded at authorization time from the
  client's signed auth entry and is NOT an argument to `settle`. The facilitator cannot
  redirect because it never supplies the destination.

### How the facilitator is prevented from redirecting funds

- **EVM:** The Permit2 witness pattern binds `witness.to`. The `x402Permit2Proxy`
  enforces recipient correctness on-chain.
- **SVM:** The channel distribution is fixed at `open`. The program re-checks at
  `distribute`. The facilitator (as zero-share `payee`) has no claim on settled funds.
  From the SVM spec: "The facilitator can close a channel at its current settled
  watermark; it cannot redirect funds or settle any nonzero amount on its own."
- **Stellar (proposed):** The contract stores `to` at authorize time and enforces it
  at settle time. The facilitator is a spender, not a holder (ADR-002 §4: "No custody
  — the contract is a spender, never a holder").

---

## Research Question 4: Timing

### validAfter semantics

From `scheme_upto.md`:

> "**Start time** (`validAfter`): Authorization is not valid before this timestamp"

- **EVM:** `permit2Authorization.witness.validAfter` — checked at verify time
  (EVM spec §Phase 3, step 5: "Verify the `deadline` (not expired) and
  `witness.validAfter` (active).")
- **SVM:** `validAfter` is in `extra` of `PaymentRequirements` and also in the
  `UptoPayload`. From the SVM spec: "`validAfter` is offchain verify-time policy.
  Neither value is client-bound; the client signs only `open`."
- **Stellar (proposed):** Would be a field in the authorization record, checked at
  both `authorize` and `settle` time.

### deadline/expiry semantics

From `scheme_upto.md`:

> "**End time** (`deadline`): Authorization expires after this timestamp"

- **EVM:** `permit2Authorization.deadline` — enforced by Permit2 on-chain. From the
  EVM spec: "Verify the `deadline` (not expired)."
- **SVM:** `expiresAt` — signed by `receiverAuthorizer` into the voucher and enforced
  by the payment-channels program (`now < expiresAt`). From the SVM spec:
  "Although the program supports `expires_at == 0` as no expiry, SVM `upto` MUST
  reject `expiresAt == 0`."
- **Stellar (proposed):** Two expiry concepts (ADR-002 §4):
  1. `signatureExpirationLedger` on the auth entry (~12 ledgers, ~60s).
  2. The authorization record's own `expiry` (longer, bounds metering window).

### How long an authorization remains usable

- **EVM:** From creation until `deadline` (Unix timestamp) or nonce consumption,
  whichever comes first.
- **SVM:** From `validAfter` until `expiresAt`, bounded by
  `maxChannelLifetimeSecs`. The SVM spec adds: "Facilitators MAY reject
  verify/deposit above `maxChannelLifetimeSecs`."
- **Stellar (proposed):** From `authorize` until the authorization record's `expiry`.
  The `signatureExpirationLedger` is a separate, shorter bound on the signed auth
  entry.

### What happens between authorization and settlement

- **EVM:** Nothing on-chain happens between the verify and settle HTTP calls. The
  Permit2 approval is a one-time setup; the actual `permitWitnessTransferFrom` call
  happens at settle time.
- **SVM:** Between deposit settle (`open`) and claim settle (`settle_and_seal` +
  `distribute`), the resource executes. The channel is `Open` on-chain. The server
  meters usage and signs a voucher. The client can call `request_close` as an escape
  hatch if the server never settles.
- **Stellar (proposed):** Between `authorize` and `settle`, the authorization record
  exists on-chain. Metering happens. The facilitator later calls `settle` with the
  actual amount.

### Whether settlement must happen within the same transaction

- **EVM:** Not required by the spec. The verify and settle HTTP calls are separate.
  The on-chain Permit2 call happens at settle time.
- **SVM:** Not required. The deposit and claim settle are separate HTTP calls and
  separate on-chain transactions.
- **Stellar (proposed):** Can be either. ADR-002 §4 shows both in one transaction,
  but two separate transactions are also viable (subject to auth entry expiration).

### What happens if the authorization expires before settlement

From `scheme_upto.md`:

> "Each authorization MUST have explicit validity time constraints"

- **EVM:** Permit2 rejects the transfer if `deadline` has passed. The settle fails.
  The authorization expires unused. From the EVM spec: "If the settled `amount = 0`,
  no on-chain transaction is required. The authorization simply expires unused."
- **SVM:** The program rejects the voucher if `expiresAt` has passed. The facilitator
  cannot seal. The client can use `request_close` to recover the deposit.
  From the SVM spec: "If the server does not settle, the payer can start forced close
  with `request_close`, wait the grace period, then recover unspent deposit."
- **Stellar (proposed):** ADR-002 §4: "If it lapses, the correct behaviour is that
  `settle` fails and the allowance is reclaimable by the buyer."

---

## Research Question 5: Extension Points

### Whether the upstream spec permits network-specific constructions

**Yes, explicitly.** The entire specification architecture is designed for this:

1. **Network-specific scheme specifications exist.** The chain-agnostic `scheme_upto.md`
   ends with: "Network-specific rules and implementation details are defined in the
   per-network scheme documents: EVM chains: See `scheme_upto_evm.md`." The same
   pattern exists for `exact`, `batch-settlement`, and `auth-capture`.

2. **The `extra` field is explicitly network-specific.** From the chain-agnostic spec:
   "scheme extensions; `extra`" — the `extra` field in `PaymentRequirements` is
   designed for network-specific and scheme-specific information.

3. **The specs template creates network-specific docs.** `scheme_impl_template.md`
   provides a template for network-specific implementation specs, indicating the
   architecture expects them.

4. **The core spec defines extension points.** From `x402-specification-v2.md` §6.1:
   "extra.scheme-specific additional information" — the `extra` field is reserved for
   this purpose.

5. **Different networks use fundamentally different constructions.** EVM uses Permit2
   with `permitWitnessTransferFrom`. SVM uses payment channels with escrow, vouchers,
   and `settle_and_seal` + `distribute`. These are structurally very different but
   both conform to the five core `upto` requirements.

### Whether a Stellar implementation may differ structurally

**Yes.** The upstream spec's architecture explicitly supports this. The five core
properties from `scheme_upto.md` are:

1. Single-use authorization
2. Time-bound authorization
3. Recipient binding
4. Maximum amount enforcement
5. Phase-dependent `amount` semantics

A Stellar implementation must enforce these five properties using Stellar-native
mechanisms (Soroban auth entries, SEP-41 token transfers, on-chain authorization
records). The structural approach (two invocations, authorization-binding contract,
escrow flow) is a valid implementation choice, provided the five properties hold.

The SVM `upto` spec demonstrates that a fundamentally different construction (payment
channels, vouchers, escrow flow) can satisfy the same chain-agnostic requirements.
The Stellar two-invocation construction is another such variation.

### Requirements for equivalent security properties

From `scheme_upto.md`:

> "Other networks MUST implement equivalent replay protection."
> "Other networks MUST implement equivalent time bounds."
> "Other networks MUST implement equivalent recipient binding."

These are MUST-level requirements on the **properties**, not on the **mechanism**.
A Stellar implementation must provide equivalent security properties using
Stellar-native primitives, which is exactly what ADR-002 §4 proposes.

---

## Summary of Key Findings

1. **The two-invocation Stellar construction is VIABLE.** It is structurally
   analogous to the SVM `upto` escrow flow and is explicitly permitted by the
   upstream protocol spec (core spec §7.2).

2. **The protocol does NOT require one settlement call.** It explicitly permits
   multiple settles (core spec §7.2) and the SVM implementation uses two.

3. **The five core `upto` properties** (single-use, time-bound, recipient binding,
   max enforcement, phase-dependent amount) are the normative requirements. The rest
   is implementation-specific.

4. **The upstream spec architecture explicitly supports network-specific constructions**
   through per-network scheme documents, the `extra` field, and the scheme template.

5. **The actual settlement amount** is communicated via `PaymentRequirements.amount`
   at settlement time, supplied by the resource server. It is NOT a separate payload
   field.

6. **The upstream specs are complete for EVM and SVM.** Both are finalized
   implementation specifications, not drafts.
