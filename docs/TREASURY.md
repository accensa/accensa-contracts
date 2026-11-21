# Treasury: Storage Layout & Disbursement Architecture

## Overview

The `Treasury` contract manages token vesting allocations, strategy yield portfolios, protocol buybacks, and fee liquidations (issue #444, #465, #466, #467, #1015). It serves as the primary capital coordination hub for the protocol.

## Storage Key Layout

The contract maintains twelve storage key variants in `DataKey` (`contracts/treasury/src/lib.rs:76`):

| Storage Key | Storage Type | Data Type | Description | Accessors (Getters) | Mutators |
|---|---|---|---|---|---|
| `Admin` | Instance | `Address` | The governance admin account | `get_admin` | `initialize` |
| `Token` | Instance | `Address` | The primary vested token (SEP-41) | `get_token` | `initialize` |
| `Schedule(Address)` | Persistent | `VestingSchedule` | A beneficiary's vesting schedule parameters | `get_schedule` | `create_schedule`, `claim_vested` |
| `WhitelistedStrategy(Address)` | Instance | `bool` | Whether an external yield strategy is whitelisted | N/A | `whitelist_strategy`, `remove_strategy` |
| `Allocation(Address)` | Instance | `StrategyAllocation` | Strategy target weight, capital cap, and deployed amount | `get_allocation`, `get_allocations` | `set_allocation`, `deploy_capital`, `recall_capital` |
| `AllocationOrder` | Instance | `Vec<Address>` | Ordered list of allocated strategies | `get_allocations` | `set_allocation`, `remove_strategy` |
| `ReserveBps` | Instance | `u32` | Share of treasury balance that must remain liquid (basis points) | `get_reserve_bps` | `set_reserve_bps` |
| `ReentrancyLock` | Instance | `()` | Reentrancy guard held across strategy interactions | N/A | `reentrancy::ReentrancyGuard` |
| `BuybackConfig` | Instance | `BuybackConfig` | Governance buyback configuration parameters | `get_buyback_config` | `configure_buyback` |
| `StableToken` | Instance | `Address` | Primary stablecoin liquidated fee assets are swapped into | `get_stable_token` | `configure_liquidation` |
| `Amm` | Instance | `Address` | Admin-approved AMM contract for fee liquidation and buybacks | `get_amm` | `configure_liquidation` |
| `PriceFeed` | Instance | `Address` | Oracle price feed bounding liquidation slippage | `get_price_feed` | `configure_liquidation` |
| `DistributionConfig` | Instance | `DistributionConfig` | Staking and yield distribution parameters | N/A | `configure_distribution` |
| `DistributionState` | Instance | `DistributionState` | Global yield accumulator and total staked tracking | `get_distribution_state`, `get_accumulator` | `distribute_yield`, `stake`, `unstake` |
| `UserDistribution(Address)` | Persistent | `UserDistribution` | User staking balance and reward debt checkpoints | `get_user_distribution` | `stake`, `unstake`, `claim_rewards` |

## Disbursement Entry Points

The contract provides three distinct value-disbursing entry points, each with specific authorization and state mutation boundaries:

### 1. `claim_vested`
* **Entry Point:** `claim_vested(beneficiary: Address)` (`contracts/treasury/src/lib.rs:337`)
* **Purpose:** Allows a vesting beneficiary to withdraw tokens that have unlocked per their schedule.
* **Authorization:** Requires `beneficiary.require_auth()`.
* **Storage Mutated:** `DataKey::Schedule(beneficiary)` updates `claimed` amount.
* **Transfers:** Transfers unlocked tokens from the treasury contract directly to `beneficiary`.

### 2. `execute_buyback`
* **Entry Point:** `execute_buyback(caller: Address)` (`contracts/treasury/src/lib.rs:424`)
* **Purpose:** Swaps accrued protocol fees for governance tokens and burns them.
* **Authorization:** Permissionless (can be invoked by any caller/keeper once threshold conditions are met).
* **Storage Read/Mutated:** Reads `DataKey::BuybackConfig`, `DataKey::Amm`, `DataKey::PriceFeed`.
* **Transfers:** Swaps fee assets via the AMM and transfers governance tokens to the burn address.

### 3. `liquidate_fees`
* **Entry Point:** `liquidate_fees(caller: Address, token_in: Address, amount_in: i128)` (`contracts/treasury/src/lib.rs:628`)
* **Purpose:** Converts volatile fee tokens into the protocol's primary stablecoin.
* **Authorization:** Admin authorization required.
* **Storage Read/Mutated:** Reads `DataKey::StableToken`, `DataKey::Amm`, `DataKey::PriceFeed`.
* **Transfers:** Sends `token_in` to AMM and receives `StableToken` bounded by the oracle slippage floor.
