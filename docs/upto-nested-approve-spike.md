# Spike: Can One Soroban Auth Entry Cover a Nested Approve?

> **Status: RESOLVED — Yes, with sub-invocation in the auth tree.**
> Research for ADR-002 §6.2. Issue #62.

## 1. Question

In the ADR-002 §4 `upto` construction, the buyer signs a single auth entry that
covers both the parent `authorize()` call and its nested `token.approve()`
sub-invocation. Does Soroban actually allow this, or does it require two
separate signatures?

## 2. Environment

| Component | Version / Detail |
|---|---|
| `soroban-sdk` | 27.0.4 |
| `soroban-env-host` | 27.0.1 |
| Rust | 1.97.1 stable |
| Platform | Windows x86_64-pc-windows-gnu |
| Test mode | `mock_all_auths_allowing_non_root_auth()` (recording) + `mock_auths()` (enforcing) |

### Test infrastructure notes

Soroban's test utilities offer three auth simulation modes:

| Mode | API | Behavior |
|---|---|---|
| **Recording** | `env.mock_all_auths()` | All `require_auth()` calls succeed and are recorded. `env.auths()` returns the captured tree. |
| **Recording (non-root)** | `env.mock_all_auths_allowing_non_root_auth()` | Same as recording, but also captures auth for addresses that are not the root transaction sender. Required when a nested call authorizes on behalf of a third party. |
| **Enforcing** | `env.mock_auths(&[...])` | Registers a mock `__check_auth` contract at each address. Auth entries must match exactly — mismatches are rejected with `InvalidAction`. |

The spike uses recording mode to capture auth tree structures and enforcing
mode to prove negative controls (mismatched trees are rejected).

## 3. Spike contract

Minimal contract with two functions:

```rust
pub fn authorize(env, payment_id, from, to, cap, expiry) -> Result<(), SpikeError> {
    from.require_auth();  // ← gates the nested approve

    let token_client = token::Client::new(&env, &token_addr);
    token_client.approve(&from, &env.current_contract_address(), &cap, &expiry);

    env.storage().persistent().set(&payment_id, AuthRecord { from, to, cap, expiry, consumed: false });
    Ok(())
}

pub fn settle(env, payment_id, actual) -> Result<(), SpikeError> {
    // ... validation ...
    token_client.transfer_from(&self, &record.from, &record.to, &actual);
    token_client.approve(&record.from, &self, &0, &0);  // clear allowance
    record.consumed = true;
    // ...
}
```

Key implementation detail: `from.require_auth()` is called **before** the
nested `token_client.approve()`. Without this, Soroban refuses the nested
approve because no auth entry covers `from`'s authorization for the token
call. This was the root cause of all initial test failures (7 of 12 original
tests failed with `Error(Auth, InvalidAction)`).

## 4. Results

### 4.1 Auth tree structure — THE ANSWER

**Yes, one signed auth entry covers both calls.** The recorded auth tree:

```
Payer: Contract(CAAA...D2KM)
Root invocation: Contract((Contract(CAAA...TA4), Symbol(authorize), [...]))
Sub-invocations count: 1
  Sub[0]: Contract((Contract(CBUS...IUNF), Symbol(approve), [...])))
```

The payer's single auth entry contains:
- **Root**: `authorize(payment_id, from, to, cap, expiry)` on the UptoAuthorization contract
- **Sub-invocation**: `approve(from, spender=contract, amount=cap, expiry)` on the SEP-41 token

The payer's signature commits to **both** the `authorize` arguments AND the
exact `approve` arguments (spender, amount, expiry). This is a binding
commitment — the buyer cannot later claim they authorized a different approve.

### 4.2 Negative controls — enforced auth tree matching

| Test | What it proves | Result |
|---|---|---|
| **Missing sub-invocation** | Auth tree without `approve` as sub-invocation is rejected | Rejected (`InvalidAction`) |
| **Wrong approve amount** | Auth tree with `approve(amount=999_999)` when actual call is `1_000_000` is rejected | Rejected (`InvalidAction`) |
| **Wrong approve spender** | Auth tree with `approve(spender=wrong_address)` is rejected | Rejected (`InvalidAction`) |

These three tests prove Soroban enforces exact argument matching on the
sub-invocation. A malicious or buggy facilitator cannot submit an auth tree
that grants less than the contract actually uses, nor redirect the approve
to a different spender.

### 4.3 Business logic — full flow verified

| Test | What it proves | Result |
|---|---|---|
| Full flow (authorize → settle) | Allowance set correctly, transfer executes, allowance cleared | Pass |
| Double settle rejected | Second `settle()` on same `payment_id` fails with `AlreadyConsumed` | Pass |
| Exceed cap rejected | `settle(amount > cap)` fails with `AmountExceedsCap` | Pass |
| Settle without authorize | `settle()` on nonexistent `payment_id` fails with `NotSettled` | Pass |
| Zero cap rejected | `authorize(cap=0)` fails with `AmountExceedsCap` | Pass |
| State inspection | On-chain `AuthRecord` matches call arguments exactly | Pass |

### 4.4 Budget measurements

| Construction | CPU instructions | Memory bytes |
|---|---|---|
| **Nested** (authorize calls token.approve) | 209,231 | 90,553 |
| **Separate** (token.approve + authorize independently) | 213,927 | 89,949 |

The nested construction is actually **~2% cheaper** in CPU than separate
invocations. This is likely because the nested path avoids redundant setup
overhead. The memory difference is negligible (< 1%).

**Conclusion: nested construction has no measurable cost penalty.**

## 5. Security implications

### What the buyer's signature commits to

When the buyer signs the auth entry, they are signing a tree that includes:

1. `authorize(payment_id, buyer, seller, cap, expiry)` — binding to recipient
2. `approve(buyer, upto_contract, cap, expiry)` — binding the approve arguments

This means:
- The **recipient** (`seller`) is cryptographically bound at auth time — the
  facilitator cannot redirect the payment.
- The **cap** is committed in both the `authorize` args and the `approve` args —
  cannot be inflated after signing.
- The **spender** (UptoAuthorization contract address) is committed — the
  approve cannot be redirected to a different contract.
- The **expiry** is committed — the allowance cannot outlive the agreed window.

### What a malicious facilitator cannot do

1. **Cannot settle for more than cap** — enforced by contract logic + approve amount.
2. **Cannot redirect to a different recipient** — `to` is in the auth entry and not
   an argument to `settle`.
3. **Cannot split into multiple settlements** — `consumed` flag prevents double-settle.
4. **Cannot modify the approve arguments** — Soroban enforces exact matching of the
   sub-invocation in the auth tree.

### Trust model

The buyer trusts:
- The UptoAuthorization contract code (auditable).
- The SEP-41 token's `approve` and `transfer_from` semantics.
- The facilitator to submit the correct auth tree (enforced by Soroban — the
  facilitator cannot forge or modify the buyer's signature).

The buyer does NOT trust:
- The facilitator with redirect authority (recipient is bound).
- The facilitator with unlimited spending (cap is committed).

## 6. Recommendation

**VIABLE — proceed with nested auth construction in ADR-002 §4.**

The experiment conclusively demonstrates that:
1. One Soroban auth entry CAN cover a parent invocation with a nested approve.
2. The sub-invocation's arguments are fully committed by the buyer's signature.
3. Soroban enforces exact matching — mismatched trees are rejected.
4. The nested construction has no cost penalty vs. separate invocations.

The only implementation requirement is that `from.require_auth()` must be
called **before** the nested `token_client.approve()` in the `authorize`
function. Without this, Soroban refuses the nested approve because no auth
entry exists for `from` at that point in the invocation.

## 7. Files

| File | Purpose |
|---|---|
| `spikes/upto-nested-approve/src/lib.rs` | Spike contract: `UptoAuthorization` with `authorize` + `settle` |
| `spikes/upto-nested-approve/src/test.rs` | 13 tests: auth tree inspection, positive flow, negative controls, budget |
| `spikes/upto-nested-approve/Cargo.toml` | Package config (crate-type `rlib`, no `cdylib`) |
| `spikes/upto-nested-approve/test_snapshots/` | 13 test snapshot JSON files |
