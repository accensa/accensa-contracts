# Scheme: `upto` on `Stellar`

> [!IMPORTANT]
> **Status: DRAFT — not yet validated, open proposal for community review.**
>
> This Stellar binding of the `upto` scheme has **not** been upstream-adopted, has **no**
> shipped reference implementation, and proposes a **two-invocation construction** whose
> per-payment cost has **not yet been measured** on testnet. This document mirrors the file
> and section register of the existing scheme specs (`scheme_exact_stellar.md`,
> `scheme_upto_evm.md`) so it can be reviewed against its siblings, but several sections
> are explicitly pending work tracked upstream as blockers:
>
> - **#64 (construction decision)** — the exact invocation sequence and what is
>   authorised at each step is still under investigation (see
>   [`ADR-002`](ADR-002-upto-scheme.md) for the open questions).
> - **#65 (sub-invocation validation)** — `exact` refuses nested `subInvocations`
>   outright; this binding nests an `approve` under its entry call, so the permitted
>   sub-invocation and its validation MUST be decided and confirmed against Soroban
>   authorization semantics before this is adopted. This is the highest-risk open item.
> - **#66 (cost measurements)** — the numbers in [§ Cost](#cost) are **estimates**
>   (marked TBD), not yet measured on testnet. The two-invocation overhead is the single
>   most important number reviewers must see measured.
> - **Reference implementation** — there is none yet; this repo's `ReceiptAnchor` /
>   `RefundVault` contracts exercise the same TTL/rent/eviction machinery but are not an
>   `upto` implementation. [§ Reference implementation](#reference-implementation)
>   links what exists.
>
> Do not cite this document as a design that works; cite it as the design being
> investigated. It is published here for review before any submission to the x402
> Technical Steering Committee.

## Versions supported
- ❌ `v1` - we don't plan to support v1 for now.
- ✅ `v2` (proposed)

## Supported Networks
This spec uses [CAIP-2](https://namespaces.chainagnostic.org/stellar/caip2) identifiers,
matching `scheme_exact_stellar.md`:
- `stellar:pubnet` — Stellar mainnet
- `stellar:testnet` — Stellar testnet

> [!NOTE]
> **Scope:** This spec covers [SEP-41]-compliant Soroban tokens **only**. Classic Stellar
> assets are not supported, and neither are allowances issued through any non-SEP-41
> interface.

## Summary
The x402 `upto` scheme authorises a transfer of **up to a maximum amount** and settles
for the **actual amount used** once the resource has been consumed. It is the scheme for
metered services — LLM per-token billing, per-byte transfer, dynamic compute — where the
price is not known when the request is made. The Client signs a ceiling; the Resource
Server meters; the Facilitator settles the actual amount, which MUST be `<=` that ceiling.

On Stellar the Client cannot sign a payment whose amount is not yet known, because a
Soroban auth entry commits to the full argument list of the invocation it authorises —
there is no "leave one argument blank" signature. `exact`'s
`transfer(from, to, amount = exact)` therefore cannot express a ceiling.

**Proposed construction:** split authorisation from settlement across two invocations,
bound by on-chain state in a thin, non-custodial `UptoAuthorization` contract:

1. **`authorize(payment_id, from, to, cap, expiry)`** — the Client signs one auth entry
   covering the `authorize` call. The contract records `{from, to, cap, expiry,
   consumed: false}` and, in the same invocation, issues a SEP-41
   `approve(from, spender = UptoAuthorization, amount = cap, live_until_ledger = expiry)`
   **as a sub-invocation** (this is the part #65 must resolve — see
   [Sub-invocation](#3-sub-invocation-authorize--approve-must-but-pending-65)).
2. **`settle(payment_id, actual)`** — the Facilitator invokes after metering. The
   contract asserts `!consumed`, `actual <= cap`, and not expired, then performs
   `transfer_from(spender = UptoAuthorization, from, to = to, amount = actual)`. `to` is
   recorded at authorisation time and is **not** an argument to `settle`, so the
   Facilitator cannot redirect the payment. The `consumed` flag is set in the same
   invocation that moves funds, making settlement single-shot, and the residual allowance
   is zeroed in the same call.

Funds move directly `from` (Client) `to` (payTo). The contract is a **spender**, never a
holder — it is non-custodial.

## Protocol Flow
The protocol flow is Client-driven with Facilitator-sponsored execution, mirroring
`exact`. The difference is that authorisation and settlement are two invocations.

1. **Client** makes a request to a **Resource Server**.
2. **Resource Server** responds with `402 Payment Required` and a `PaymentRequired` header
   whose `amount` is the **maximum** it will accept for this request, plus
   `extra.areFeesSponsored: true`.
3. **Client** builds the `authorize` invocation, simulates it to identify the required
   authorization entries, and signs them with their wallet, setting `signatureExpirationLedger`
   to `currentLedger + ledgerTimeout` (`ledgerTimeout = ceil(maxTimeoutSeconds /
   estimatedLedgerSeconds)`; fallback `5` seconds per the `exact` spec).
4. **Client** serializes the signed invocation as XDR (base64) and sends it in the
   `PaymentPayload` to the **Resource Server**.
5. **Resource Server** forwards the `PaymentPayload` and `PaymentRequirements` (with
   `amount` = the authorized maximum) to the **Facilitator Server's** `/verify`
   endpoint.
6. **Facilitator** decodes and validates the invocation, its auth entries, the
   sub-invocation (per [Sub-invocation](#3-sub-invocation-authorize--approve-must-but-pending-65)), the cap
   against `requirements.amount`, the recipient binding, and the expiry clocks.
7. **Facilitator** returns a `VerifyResponse`.
8. **Resource Server** serves the request and meters actual usage.
9. **Resource Server** forwards the payload to the Facilitator's `/settle` endpoint with
   `PaymentRequirements.amount` now set to the **actual** metered amount.
   - NOTE: `/settle` MUST perform full verification independently and MUST NOT assume
     prior verification.
10. **Facilitator** re-verifies the signature against the **authorized maximum**, validates
    `actual <= maximum`, then builds and submits the `settle` invocation from its own
    account as transaction source.
11. **Facilitator** simulates, signs, and submits via RPC `sendTransaction`, then polls for
    confirmation and responds with a `SettlementResponse` carrying the actual `amount`.
12. **Resource Server** grants the **Client** access to the resource.

## Gap in the `upto` guarantee versus `exact` on Stellar
`exact` accomplishes everything in **one** on-chain `transfer`. `upto` needs **two**
on-chain invocations (authorise + settle). This roughly doubles the on-chain overhead per
metered payment versus `exact` and is the principal cost objection to the scheme on
Stellar — it is called out honestly in [§ Cost](#cost) and must be measured before
adoption.

## `PaymentRequirements` for `upto`
In addition to the standard x402 `PaymentRequirements` fields, the `upto` scheme on
Stellar requires the following inside the `extra` field:

> The `amount` field is **phase-dependent**, consistent with `scheme_upto.md` and
> `scheme_upto_evm.md`: at **verify** time it is the **maximum** the Client authorizes;
> at **settle** time it is the **actual** amount to charge (MUST be `<=` the maximum).

```json
{
  "scheme": "upto",
  "network": "stellar:testnet",
  "amount": "10000000",
  "asset": "CBIELTK6YBZJU5UP2WWQEUCYKLPU6AUNZ2BQ4WWFEIE3USCIHMXQDAMA",
  "payTo": "GBHEGW3KWOY2OFH767EDALFGCUTBOEVBDQMCKU4APMDLQNBW5QV3W3KO",
  "maxTimeoutSeconds": 300,
  "expiryLedger": 0,
  "extra": {
    "areFeesSponsored": true
  }
}
```

**Field Definitions:**
- `extra.areFeesSponsored`: Whether the facilitator sponsors fees. Always `true` in the
  current proposal; a non-sponsored flow may be added later. **Pending validation:** the
  allowance flow must be confirmed to preserve zero-XLM-for-payer (see
  [Transaction fees → Fee sponsorship](#transaction-fees)).
- `expiryLedger`: the ledger at which the on-chain authorization record (and its
  SEP-41 allowance) lapses. This is the *second, on-chain* clock — distinct from the auth
  entry's `signatureExpirationLedger` (see [Expiry semantics](#expiry-semantics)).

## PaymentPayload `payload` Field
The `payload` field of the `PaymentPayload` contains:

```json
{
  "invocation": "AAAAAgAAAABriIN4poutFUmHfB6FbFJu8GgXoPPTGQWREqFpPfvO1AAAAAAAAAAAAAAAAAAAAA...",
  "paymentId": "a5c9...",
  "expiryLedger": 0
}
```

- `invocation`: base64-encoded XDR of the Client's signed `authorize` invocation
  (host fn `authorize` on the `asset`-side `UptoAuthorization` contract — see
  [Wire shape](#wire-shape-for-settle) for how the settle half differs).
- `paymentId`: the opaque identifier the Client and Facilitator both use to bind
  `/verify` to `/settle`. Whether upstream requires `paymentId` to be derived
  deterministically (e.g. from the authorised invocation) is a #64/#65 open item.
- `expiryLedger`: mirrors `PaymentRequirements.expiryLedger`; the on-chain expiration
  clock.

**Full `PaymentPayload` object:**

```json
{
  "x402Version": 2,
  "resource": {
    "url": "https://api.example.com/llm/generate",
    "description": "LLM text generation endpoint",
    "mimeType": "application/json"
  },
  "accepted": {
    "scheme": "upto",
    "network": "stellar:testnet",
    "amount": "10000000",
    "asset": "CBIELTK6YBZJU5UP2WWQEUCYKLPU6AUNZ2BQ4WWFEIE3USCIHMXQDAMA",
    "payTo": "GBHEGW3KWOY2OFH767EDALFGCUTBOEVBDQMCKU4APMDLQNBW5QV3W3KO",
    "maxTimeoutSeconds": 300,
    "expiryLedger": 0,
    "extra": {
      "areFeesSponsored": true
    }
  },
  "payload": {
    "invocation": "AAAAAgAAAABriIN4poutFUmHfB6FbFJu8GgXoPPTGQWREqFpPfvO1AAAAAAAAAAAAAAAAAAAAA...",
    "paymentId": "a5c9...",
    "expiryLedger": 0
  }
}
```

### Wire shape for `/settle`
At settle, the Facilitator does **not** forward the Client's signed `authorize`
invocation to be replayed. Instead `/settle` carries the same `PaymentRequirements` (with
`amount` = the actual metered amount), the original `paymentId`, and the Client's
`authorize` auth-entry material **only as a signature reference for re-verification**.
The on-chain work at settle is a new `settle(payment_id, actual)` invocation built by the
Facilitator from its own account. The full settle wire shape, and whether it re-sends the
Client's XDR or a compact reference, is a #64/#65 detail not yet fixed.

## Facilitator Verification Rules (MUST)
A facilitator verifying an `upto` scheme on Stellar MUST enforce all of the following
before sponsoring and signing any transaction. These rules are written to be reproducible
by a second implementer without further contact.

### 1. Protocol Validation
- The `x402Version` MUST be `2`.
- Both `payload.accepted.scheme` and `requirements.scheme` MUST be `"upto"`.
- The `payload.accepted.network` MUST match `requirements.network`.
- The `payload.accepted.asset` MUST equal `requirements.asset`.
- The `payload.accepted.payTo` MUST equal `requirements.payTo`.

### 2. Authorise invocation structure (MUST)
At `/verify`, the Client's signed invocation MUST:
- Contain exactly **1** operation of type `invokeHostFunction`, function type
  `hostFunctionTypeInvokeContract`.
- Target the known `UptoAuthorization` contract address, and the function name MUST be
  `"authorize"` with **exactly 5 arguments**:
  - Argument 0 — `payment_id`: MUST equal `payload.payload.paymentId`.
  - Argument 1 — `from`: the address signing the auth entries; MUST be the payer.
  - Argument 2 — `to`: MUST equal `requirements.payTo` exactly (recipient binding).
  - Argument 3 — `cap`: MUST equal `requirements.amount` (the authorized maximum) exactly
    (as i128).
- Argument 4 — `expiry`: MUST equal `requirements.expiryLedger`.

> **#64 note:** whether `authorize` also takes `expiry` as an explicit argument, or reads
> it from `live_until_ledger`, is not yet fixed. The above assumes an explicit argument.

### 3. Sub-invocation: `authorize` → `approve` (MUST, but pending #65)
This is the one place the Stellar `upto` binding deliberately diverges from `exact`,
which refuses nested `subInvocations` outright ("The `rootInvocation` MUST NOT contain
`subInvocations` that authorize additional operations"). For `upto` to grant the
contract an allowance without a second Client transaction, the `authorize` root
invocation MUST contain a **single permitted sub-invocation** auth entry:

- The `rootInvocation` MUST be the `UptoAuthorization.authorize` call from argument set in
  [§2](#2-authorise-invocation-structure-must).
- It MUST contain **exactly one** `subInvocations` entry, and that entry MUST be the
  SEP-41 `approve(from, spender, amount, live_until_ledger)` protected function on
  `requirements.asset`, where:
  - `from` = the `from` in the root invocation,
  - `spender` = the `UptoAuthorization` contract address,
  - `amount` = the `cap` (the authorized maximum),
  - `live_until_ledger` = `expiry` (the on-chain expiry clock).
- The sub-invocation MUST NOT itself contain further `subInvocations` (no nesting past one
  level).
- The auth entries valid for the sub-invocation MUST be signed by the same `from` as the
  root.

> [!WARNING]
> **Highest-risk open item (#65).** `exact` refuses *any* sub-invocation. This binding
> *requires* exactly one well-formed `approve` sub-invocation. Until #65 is resolved and
> confirmed against Soroban's authorization semantics, a conforming implementation MUST
> reject a payload with **zero** or **more than one**, well-specified sub-invocation, and
> the whole construction remains subject to change. If Soroban cannot express this in a
> single Client-signed auth tree, a fallback (two auth entries / two signatures, surfaced
> in [Alternatives](#alternatives)) MUST be used instead and this section rewritten.

### 4. Authorization entries (MUST)
- The transaction MUST contain signed authorization entries for the `from` address.
- Auth entries MUST use credential type `sorobanCredentialsAddress` only.
- The auth-entry `signatureExpirationLedger` MUST NOT exceed
  `currentLedger + ceil(maxTimeoutSeconds / estimatedLedgerSeconds)` (fallback `5`s).
- All required signers MUST have signed.

### 5. Facilitator safety (MUST)
- The transaction source account provided by the Client MUST NOT be the facilitator's
  address.
- The operation source account provided by the Client MUST NOT be the facilitator's
  address.
- The facilitator MUST NOT be the `from` in the transfer.
- The facilitator address MUST NOT appear in any authorization entries.
- The simulation MUST emit events showing **only** the expected balance changes
  (recipient increase, payer decrease) with NO OTHER BALANCE CHANGES.

### 6. Simulation (MUST)
- The facilitator MUST re-simulate the Client's `authorize` invocation against the
  current ledger state; the simulation MUST succeed and yield the expected approval event.

### 7. Re-verification at `/settle` (MUST)
Independently of any prior `/verify`:
- Re-verify the Client's signature against the **authorized maximum**
  (`cap` / `requirements.amount` / `permitted.amount`), NOT against the settle-time
  `requirements.amount`, matching `scheme_upto_evm.md` [Settle-Time Verification].
- Validate `settle.actual <= authorized maximum`.
- Validate the on-chain record is `!consumed` and not past `expiry`.
- The `to` from the on-chain record MUST equal `requirements.payTo`.

## Expiry semantics
There are **two independent clocks** on Stellar, and conflating them is a known bug
vector. They are named and distinguished here:

| Clock | Source | Bounds | Failure mode if conflated |
|---|---|---|---|
| **`signatureExpirationLedger`** | the auth entry over `authorize` | `<= currentLedger + ceil(maxTimeoutSeconds / estimatedLedgerSeconds)` (~60s at defaults). Bounds how long the *signed authorization* is submittable. | Deleting it lets a stale signed auth be reused later, or rejects it before the work completes. |
| **on-chain `expiry`** (`live_until_ledger` + record `expiry`) | stored in `UptoAuthorization` at `authorize`; passed as `expiryLedger` | Bounds how long after `authorize` a `settle` may occur; MUST be long enough for the metered work. | Treating the short auth-expiry as the record expiry rejects valid settlements; treating the long record expiry as the auth expiry lets stale authorizations be submitted. |

- If the on-chain `expiry` lapses before `settle`, the correct behaviour is that `settle`
  fails and the residual allowance is reclaimable by the `from` (the allowance itself is
  already bounded by `live_until_ledger` and by zeroing on settle).
- **TBD:** the exact relationship between `maxTimeoutSeconds`, `signatureExpirationLedger`,
  and the on-chain `expiry` (must the client set record-expiry ≥ auth-expiry? how long for
  a metered session?) is a #64/#65 decision.

## Cost
> **Pending #66 — these are estimates, not measurements.** No `upto` settlement has been
> priced on testnet yet. Costs below are an order-of-magnitude projection from the
> `exact` Stellar binding plus the two-invocation overhead; they must be redone from a
> running implementation.

Authorship: this half is load-bearing in a way the rest of this repo is not — the
`UptoAuthorization` record is a persistent entry that carries rent and can be evicted. The
repo's `ReceiptAnchor`/`RefundVault` work (TTL strategy, `extend_*` public TTL calls,
`prune_*` paths) is the template, and **#66** must measure:

- Per-`authorize` on-chain cost (simulation + fee from a live testnet submission).
- Per-`settle` on-chain cost.
- **Two-invocation overhead:** the delta per metered payment versus `exact`'s single
  transfer — `~2x` on-chain cost is the expectation and the principal objection to `upto`
  on Stellar. This is the number reviewers will find first, so it should come from a
  measurement, not from this estimate.
- Rent / TTL extension cost implied by leaving the authorization record persistent for its
  expiry window, and who pays it. The natural payer is the Facilitator (it benefits the
  most from the authorization existing), but this needs costing. If the authorization is
  **temporary storage** keyed to its own expiry (per `ADR-002` §4), then rent is bounded by
  the expiry window rather than persisting.

> [!NOTE]
> The RFP's premise is that sub-cent fees make per-payment settlement viable; roughly
> doubling that line item is a real objection and must be priced, not waved past. Until
> #66 lands, treat `upto` on Stellar as economically unproven.

## Error taxonomy
Error codes follow the x402 specification's error handling, with scheme/network-specific
codes in the same register as `exact`. Proposed additions (consistent with
`invalid_exact_stellar_payload_fee_exceeds_maximum` and `scheme_upto_evm.md`'s
`invalid_upto_evm_payload_settlement_exceeds_amount`):

| Reason | Meaning |
|---|---|
| `invalid_upto_stellar_payload_settlement_exceeds_maximum` | settle `actual` > authorized maximum. |
| `invalid_upto_stellar_payload_authorization_consumed` | `settle` on an already-settled `paymentId`. |
| `invalid_upto_stellar_payload_authorization_expired` | on-chain record `expiry` lapsed before settle. |
| `invalid_upto_stellar_payload_bad_subinvocation` | root auth does not contain exactly the single permitted `approve` sub-invocation. |
| `invalid_upto_stellar_payload_recipient_mismatch` | on-chain `to` ≠ `requirements.payTo` / `payTo` mismatch. |
| `invalid_upto_stellar_payload_invocation_structure` | invocation does not match the authorise shape ([§2](#2-authorise-invocation-structure-must)). |
| `invalid_upto_stellar_payload_fee_exceeds_maximum` | derived settlement fee exceeds the facilitator's `maxTransactionFeeStroops` circuit breaker. |
| `invalid_upto_stellar_payload_signature_expired` | auth-entry `signatureExpirationLedger` exceeded. |

TBD against `scheme_exact_stellar.md` for the standard codes shared verbatim (e.g.
`maxTransactionFeeStroops`, default `50,000` stroops). No single-on-chain-transfer
equality check applies the way it does for `exact`; the equality check in `exact` maps to
the **cap** equality at `/verify` time only.

## Transaction Fees
Fee model mirrors `exact`: in the sponsored flow the facilitator fully controls
settlement fees and MUST NOT use the Client's fee bid.

- **Facilitator (MUST):** derive the settlement fee from a fresh simulation at settle time
  (`simulationResourceFee + inclusionBuffer`, buffer >= `100` stroops); refresh Soroban
  resource data (footprint and `resourceFee` cap) from that same simulation; fully override
  the Client's fee when rebuilding.
- **Safety ceiling:** a `maxTransactionFeeStroops` circuit breaker (default `50,000`
  stroops) — if the derived fee exceeds it, reject with
  `invalid_upto_stellar_payload_fee_exceeds_maximum`. Two invocations cost **two** fees and
  must be within the ceiling; this is part of the #66 measurement.
- **Fee sponsorship:** `extra.areFeesSponsored: true` means the client still holds only the
  payment asset. **Pending validation:** whether the `authorize` + `approve` sub-invocation
  keeps zero-XLM-for-payer under the Facilitator's sponsorship, and whether the second
  (`settle`) invocation is likewise sponsored, is an open #64/#65 item and a precondition
  for the RFP requirement.

## Settlement Logic
Settlement is the Facilitator issuing a fresh `settle(payment_id, actual)` invocation from
its own account; the Contract enforces the invariants on-chain (single settlement,
`actual <= cap`, not expired, `to` bound), so the Facilitator does not need to be trusted
for these — only for metering correctly (see [Security considerations](#security-considerations)).

### Phase 1: On-chain enforcement (MUST, contract-side)
- Assert `!consumed` (else `invalid_upto_stellar_payload_authorization_consumed`).
- Assert `actual <= cap` (else `..._settlement_exceeds_maximum`).
- Assert not past `expiry` (else `..._authorization_expired`).
- Perform `transfer_from(spender = UptoAuthorization, from, to, amount = actual)`.
- Zero the residual allowance (`approve(from, spender = UptoAuthorization, 0)`).
- Set `consumed := true`.
- All in **one** invocation so the single-settlement guarantee holds.

### Phase 2: Transaction construction and submission (Facilitator MUST)
1. Build `settle` invocation with source = facilitator account.
2. Simulate; derive settlement fee and fresh Soroban data.
3. Sign and submit via RPC `sendTransaction`; verify `PENDING`, poll to `SUCCESS`/`FAILED`.

### Phase 3: `SettlementResponse`
```json
{
  "success": true,
  "transaction": "a1b2c3d4e5f6...",
  "network": "stellar:testnet",
  "payer": "GBHEGW3KWOY2OFH767EDALFGCUTBOEVBDQMCKU4APMDLQNBW5QV3W3KO",
  "amount": "1858"
}
```
- `transaction`: the settlement transaction hash.
- `payer`: the address that paid (the client, not the facilitator).
- `amount`: the **actual** settled amount in atomic token units; MAY be `0`.
- A `$0` (zero) settlement MAY skip the on-chain transfer and simply let the authorization
  expire unused — the contract SHOULD allow `settle(paymentId, 0)` to mark the record
  `consumed` without a transfer.

## Reference implementation
**Pending — none exists yet.** There is no `UptoAuthorization` contract anywhere in this
repo or, as of the date of this document, adopted in the Stellar x402 ecosystem
(`stellar/x402-stellar` tracks the same gap in
[issue #71](https://github.com/stellar/x402-stellar/issues/71)). What exists that is
directly reusable:

- [`contracts/refund-vault`](../contracts/refund-vault) and
  [`contracts/receipt-anchor`](../contracts/receipt-anchor) — Soroban `soroban-sdk`
  contracts with real TTL/rent/eviction (`extend_*`, `prune_*`), the closest template for
  `UptoAuthorization`'s storage strategy, each with fuzz suites.
- [`ADR-002`](ADR-002-upto-scheme.md) — the construction this document proposes.

A conforming reference implementation MUST ship:
- `UptoAuthorization` contract with passing tests covering the invariants in
  [Settlement Logic Phase 1](#phase-1-on-chain-enforcement-must-contract-side); and
- a facilitator example that verifies and settles `actual <= authorized max`, as
  `stellar/x402-stellar#71` acceptance criteria specify.

## Alternatives
Kept open, per `ADR-002` §5 and `stellar/x402-stellar#71`:
- **Contract-free:** raw `approve(cap)` then `transfer_from(actual)` without a binding
  contract — strictly weaker (no recipient binding, no single-shot), but cheaper and ships
  no contract. This is a legitimate v1 **only if** the weaker trust model is documented
  explicitly. The decision is not made here.
- **Single-transaction, two-auth-entry:** batching `approve` + `transfer_from` into one
  transaction with multiple auth entries (option (c) in `stellar/x402-stellar#71`) — avoids
  a second ledger/transaction for the pay step but cannot because `transfer_from`'s amount
  is not known at signing; listed for completeness and must be resolved by #64/#65.

## Security considerations
1. **Maximum-amount risk:** the client is charged up to `cap`; the server controls the
   metered amount, so a misbehaving server can charge the cap regardless of actual usage.
   This is inherent to `upto` (see `scheme_upto_evm.md` §Security) and must be priced into
   the client's authorisation budget.
2. **Facilitator is not trusted for binding or replay:** recipient binding and
   single-settlement are enforced on-chain (the `to` recorded at `authorize`; `consumed`
   set in the same invocation as the transfer). The Facilitator is trusted only to meter
   the actual amount.
3. **Sub-invocation is the attack surface:** the single permitted `approve` sub-invocation
   is where a malformed payload could grant a different spender/amount. `exact` refuses
   sub-invocations for exactly this reason; until #65 lands, strict
   exactly-one-`approve`-of-the-right-shape validation ([§3](#3-sub-invocation-authorize--approve-must-but-pending-65))
   is mandatory, and the construction is provisional.
4. **Allowance hygiene:** a partial settlement must not leave a standing, reusable
   allowance. `live_until_ledger` bounds it in time, and settlement zeroes it in the same
   call; an unused authorization lapses via its own `expiry`.
5. **What this construction does NOT protect against:** it does not stop a facilitator from
   metering dishonestly (charging `cap` for no work); it does not protect the client's
   balance beyond `cap`; and it assumes the `UptoAuthorization` contract itself is
   deployed correctly and immutably. A logic bug in the contract is a logic bug in the
   scheme — the contract MUST be audited before any mainnet adoption.
6. **Refund interaction (`upto` ∩ `RefundVault`):** if a payment settles for less than
   `cap`, the refundable amount in this repo's `RefundVault` should be the **settled**
   amount, not the cap. Whether anything changes in `RefundVault` is a follow-up issue.

## Appendix
Key concepts shared with `scheme_exact_stellar.md` apply unchanged: the Stellar
transaction hierarchy, `invokeHostFunction` operations, fee-funded (sponsored) submission,
and the two authorization patterns (auth-entry signing vs full transaction signing). The
`upto` scheme uses **auth-entry signing** (approach #1) so C-accounts and G-accounts are
supported.

- **Record TTL / rent:** each open authorization is a persistent entry with rent and
  possible eviction. Per `ADR-002`, authorization entries are **temporary storage keyed to
  their own expiry** (unlike `RefundVault`, whose refund records must outlive the window).
  A `prune_*` path for lapsed authorizations mirrors `ReceiptAnchor.prune_batches`. Who
  pays the rent is an open #66 item.
- **Sequence-number contention:** agent traffic is bursty and the facilitator submits every
  settlement; channel accounts are the standard answer and need designing (#64#5 in
  `ADR-002`).

## Upstream submission
This document was authored in this repository (`docs/`) for review before being proposed to
the x402 Technical Steering Committee and contributed to
[`x402-foundation/x402`](https://github.com/x402-foundation/x402) under
`specs/schemes/upto/`. It is deliberately a **draft**: the construction, cost model, and
sub-invocation handling must resolve #64/#65/#66 and pass TSC review cycles before
adoption. `ADR-002` will be updated with the upstream outcome.

[SEP-41]: https://stellar.org/protocol/sep-41
[auth-entry-signing]: https://developers.stellar.org/docs/build/guides/freighter/sign-auth-entries
[sequence number]: https://developers.stellar.org/docs/learn/glossary#sequence-number