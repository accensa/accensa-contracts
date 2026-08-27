# ADR 002: The `upto` Settlement Scheme on Stellar

> **Status: DRAFT — design exploration, not an accepted decision.**
> Nothing here has been validated against the upstream `upto` specification, against a
> running contract, or against Soroban's authorization semantics in practice. §6 lists
> what must be confirmed before any of this is proposed as a network spec. Do not cite
> this document as a design that works; cite it as the design being investigated.
>
> **2026-08-26 update:** the construction below has been drafted into a Stellar scheme
> spec — [`scheme_upto_stellar.md`](scheme_upto_stellar.md) — in the upstream x402 format.
> That draft keeps this ADR's caveats live: it is published as an **open proposal for
> review**, not an adopted design, and the sections that depend on the blockers below
> (#64 construction validation, #65 sub-invocation semantics, #66 cost measurements, and a
> reference implementation) are marked pending rather than asserted. It will not be
> submitted to the x402 Technical Steering Committee until those resolve.

## 1. Context

x402 settles today on Stellar with the `exact` scheme: the buyer signs an authorization
committing to a specific amount, and the facilitator submits it. `exact` is specified for
Stellar in `scheme_exact_stellar.md`.

`upto` is the scheme for **metered** services — token billing, per-inference charges,
anything where the price is not known when the request is made. The buyer authorizes a
cap; the seller meters actual usage; the facilitator settles the actual amount, which is
at most the cap. It has EVM and SVM implementation specs. **It has no Stellar one.**

That gap is the reason this ADR exists. Authoring `scheme_upto_stellar.md` and
contributing it upstream is a deliverable of the SCF x402 Facilitator RFP, and it is the
part of that RFP where this repo's existing work — Soroban contracts with a real TTL,
rent, and eviction strategy — is directly relevant rather than adjacent.

## 2. The core problem: Soroban auth commits to every argument

This is what makes `upto` structurally harder on Stellar than a naive reading suggests.

Soroban authorization is not a pre-signed transaction. The buyer signs an **auth entry**
permitting a *specific contract invocation* — and that signature commits to the full
argument list of the call being authorized. There is no mechanism for signing an
invocation with one argument left free to be filled in later.

So the obvious construction does not exist:

```
buyer signs:  token.transfer(from, to, ???)   ← `actual` is unknown at signing time
```

The buyer signs before the work is metered. The amount is known only after. Any `upto`
design on Stellar must therefore split authorization from settlement across two
invocations, and carry the binding between them in on-chain state.

## 3. Why SEP-41 allowances alone are insufficient

The RFP asserts this and it should be understood rather than repeated. SEP-41 offers
`approve(from, spender, amount, expiration_ledger)` followed by
`transfer_from(spender, from, to, amount)`. Using that pair on its own fails the scheme's
two guarantees:

| Guarantee | Why `approve`/`transfer_from` does not provide it |
|---|---|
| **Recipient binding** | An allowance authorises a *spender*, not a *destination*. A facilitator holding an allowance can `transfer_from` to any address it likes. Nothing ties the movement to the `payTo` the buyer agreed to. |
| **Single settlement** | An allowance is a standing balance that can be drawn repeatedly until exhausted or expired. Two partial draws totalling the cap are indistinguishable, at the token layer, from one correct settlement. |

There is a third, practical objection: `approve` is a separate transaction the buyer must
submit, which means the buyer needs XLM for fees or a separate sponsorship path — against
an RFP requirement that the buyer need only hold the payment asset.

**A contract-free design is therefore possible but strictly weaker, and the RFP requires
that its weaker trust model be documented explicitly rather than glossed.** This ADR
proposes a contract.

## 4. Proposed construction: a thin authorization-binding contract

The contract holds no funds. It exists to bind a recipient and to make settlement
single-shot — the two properties the allowance primitive cannot express.

```mermaid
sequenceDiagram
    participant B as Buyer (agent)
    participant F as Facilitator
    participant U as UptoAuthorization
    participant T as SEP-41 Token

    Note over B: signs ONE auth entry covering<br/>both calls in the tree
    B->>F: PaymentPayload (cap, payTo, payment_id)
    F->>U: authorize(payment_id, from, to, cap, expiry)
    U->>T: approve(from, spender=U, cap, expiry)
    Note over U: records {from, to, cap, expiry, consumed:false}

    Note over F: ...work is metered, actual ≤ cap...

    F->>U: settle(payment_id, actual)
    U->>U: assert !consumed, actual ≤ cap, not expired
    U->>T: transfer_from(U, from, to, actual)
    U->>T: approve(from, spender=U, 0)
    Note over U: consumed := true
```

The pieces that carry the guarantees:

- **Recipient binding** — `to` is recorded at authorization time from the buyer's signed
  auth entry and is *not* an argument to `settle`. The facilitator cannot redirect the
  payment, because it never supplies the destination.
- **Single settlement** — the `consumed` flag is set in the same invocation that moves
  the funds. A second `settle` on the same `payment_id` fails.
- **No residual allowance** — settlement clears the approval to zero in the same call, so
  `cap - actual` does not linger as a standing claim on the buyer's balance.
- **No custody** — the contract is a spender, never a holder. Funds move directly from
  buyer to seller. This preserves the RFP's non-custodial requirement, which a
  cap-into-escrow design would complicate.

### Expiration

Two independent clocks, and conflating them is a likely bug:

1. **`signatureExpirationLedger`** on the auth entry, bounded by `maxTimeoutSeconds` —
   roughly 12 ledgers, about 60 seconds by default. This bounds how long the *signed
   authorization* is submittable, and is far too short to bound a metered session.
2. **The authorization record's own `expiry`**, stored on-chain. This bounds how long
   after `authorize` a `settle` may occur, and must be long enough for the metered work
   to complete. If it lapses, the correct behaviour is that `settle` fails and the
   allowance is reclaimable by the buyer.

### Storage, TTL, and rent

Each open authorization is a persistent entry, so it carries rent and can be evicted —
the failure mode this repo already handles for `ReceiptAnchor` batches and `RefundVault`
refunds. Applying the same approach:

- Authorization entries are **temporary storage keyed to their own expiry**, not
  persistent storage, since an authorization has no meaning after it lapses. This is a
  deliberate difference from `RefundVault`, where a refund record must outlive the
  refund window for auditability.
- A `prune` path for lapsed authorizations, mirroring `prune_batches`.
- **Who pays the rent must be stated.** The natural answer is the facilitator, since it
  is the party that benefits from the authorization existing, but this needs costing —
  the RFP is explicit that per-payment on-chain overhead must stay off the hot path.

## 5. Consequences

### Benefits
- Provides both guarantees the RFP names, rather than documenting their absence.
- Non-custodial: no escrow, no pooled balance, no operator holding buyer funds.
- Composes with Stellar smart-account spending policies — a `__check_auth` account can
  apply a budget policy to the `authorize` call, keeping an agent inside a spend limit
  without the facilitator needing to enforce it.

### Costs — state these plainly
- **Two on-chain invocations per metered payment instead of one.** For an RFP whose
  premise is that sub-cent fees make per-request payment viable, roughly doubling
  settlement cost is a real objection and must be priced, not waved past. *(Note: Measurement of this cost is currently pending the resolution of Issue #65 and #66; benchmarking infrastructure is in place to determine the true economic impact once the implementation exists.)*
- **It ships a Soroban contract**, which expands the security review from "an off-chain
  service and its cryptographic validation" to a contract audit. The RFP's costing note
  assumes v1 ships no new contract; proposing this changes that line item.
- Adds a second expiry concept that integrators can get wrong.

### The alternative, kept open
A contract-free variant — plain `approve`/`transfer_from` with the facilitator trusted
not to redirect or split the draw — is cheaper, ships no contract, and keeps the audit
scope small. It is a legitimate v1 if and only if the weaker trust model is documented
explicitly, as the RFP demands. **The decision between these two is not made in this
ADR.**

## 6. Open questions — resolve before proposing this upstream

1. **Does the upstream `upto` spec permit a two-invocation construction at all**, or does
   its wire format assume a single settlement call? The EVM and SVM specs must be read
   before any Stellar spec is drafted; this ADR was written from the RFP's summary of
   `upto`, not from the specs themselves.
2. **Can one signed auth entry cover both `authorize` and the nested `approve`?** The
   construction in §4 assumes the buyer signs a single auth tree with `approve` as a
   sub-invocation. This needs confirming against Soroban's authorization semantics — if
   it requires two separate buyer signatures, the UX argument for this design weakens
   considerably.
3. **[RESOLVED] What does `settle` cost**, and does the pair stay within per-transaction CPU,
   memory, read, and write limits under realistic load?
   - **Resolution:** Measurement is currently blocked by Issue #65 (`upto` contract implementation). A benchmarking skeleton and methodology have been defined in [BENCHMARKS.md](BENCHMARKS.md), which will record the `authorize`, `settle`, and pair cost against exact limits once the implementation is available.
4. **Sequence-number contention.** Agent traffic is bursty and the facilitator submits
   every settlement. Channel accounts are the standard answer; that needs designing, not
   naming.
5. **[RESOLVED] Refund interaction.** `RefundVault` in this repo keys refunds on a payment
   reference. If an `upto` payment settles for less than its cap, what is the refundable
   amount — and does anything need to change here?
   - **Resolution:** `RefundVault` enforces refund ceilings natively on-chain for metered `upto` payments. It executes a cross-contract read against the configured `SettlementContract` to verify the actual settled amount and ledger, rejecting unsettled or expired authorizations. The vault operates entirely on this verified on-chain data, completely ignoring any ceiling supplied by the caller, and correctly tracks partial refunds up to that ceiling.
6. **Does the facilitator need `authorize` at all**, or can the buyer call it directly?
   Fee sponsorship (`extra.areFeesSponsored`) suggests the facilitator submits, but that
   should follow from the spec rather than convenience.

## References
- `rfp.md` §3.4 (settlement schemes), §3.5 (Stellar-specific considerations), §3.6
  (audit scope)
- [`scheme_upto_stellar.md`](scheme_upto_stellar.md) — the drafted Stellar `upto` scheme
  spec this ADR motivates (in the upstream x402 format)
- `docs/ADR-001-merkle-structure.md` — prior ADR format
- `docs/SECURITY_MODEL.md`, `docs/storage-audit.md` — existing TTL and storage analysis
- Upstream: `x402-foundation/x402`, `specs/schemes/` — not yet read for the final form; see
  the upstream-submission note in `scheme_upto_stellar.md`. The Stellar ecosystem tracks
  the same gap in [`stellar/x402-stellar#71`](https://github.com/stellar/x402-stellar/issues/71).
