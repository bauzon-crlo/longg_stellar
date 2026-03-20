# pricebridge

A Soroban smart contract that ingests Ethereum ABI-encoded Chainlink-style price feed data, validates it, and exposes it for consumption by other Soroban contracts. Built with DeFi safety mechanisms including circuit breakers, TWAP, price normalization, and historical snapshots.

---

## Table of Contents

- [Overview](#overview)
- [Problem Statement](#problem-statement)
- [How It Works](#how-it-works)
- [DeFi Safety Features](#defi-safety-features)
- [Contract API](#contract-api)
- [Data Structures](#data-structures)
- [Error Reference](#error-reference)
- [Security Considerations](#security-considerations)
- [Development](#development)
- [Deployment](#deployment)
- [Project Structure](#project-structure)

---

## Overview

`pricebridge` acts as an on-chain price oracle layer for the Stellar/Soroban ecosystem. It accepts ABI-encoded price data in the Chainlink feed format — making it compatible with existing EVM oracle infrastructure and relayers — and stores validated prices on-chain for other Soroban contracts to consume. Each price feed is independently configured with staleness windows, sanity bounds, and deviation limits.

---

## Problem Statement

### The Oracle Problem on Stellar

Soroban-based DeFi protocols — lending markets, stablecoins, perpetuals, yield aggregators — all require reliable, manipulation-resistant price data to function correctly. Without a trusted price feed, collateral cannot be valued, liquidations cannot be triggered, and pegs cannot be maintained. Stellar currently lacks a native, standardized on-chain oracle layer comparable to what Chainlink provides on EVM chains.

### What Goes Wrong Without It

**Price manipulation.** A single price update with no deviation checks can be exploited to drain lending protocols, mint unbacked stablecoins, or avoid liquidations. Flash loan attacks on DeFi protocols have repeatedly exploited spot price oracles with no smoothing mechanism.

**Stale data.** Protocols that do not enforce staleness windows will continue operating on prices that are minutes or hours old during periods of high volatility — exactly when accurate pricing matters most.

**No cross-chain price portability.** EVM ecosystems have a mature oracle infrastructure built around Chainlink's ABI-encoded feed format. Protocols bridging assets between EVM chains and Stellar have no standardized way to relay that price data onto Soroban without custom one-off solutions.

**Lack of composability.** Price feeds across different protocols use different decimal precisions, making it impossible for a consuming contract to safely compare or aggregate prices without custom normalization logic per feed.

### How `pricebridge` Addresses These

| Problem | Solution |
|---|---|
| No on-chain oracle for Soroban | Stores validated prices on-chain, readable by any Soroban contract |
| Single-block price manipulation | TWAP smooths prices across a configurable time window |
| Flash price anomalies | Circuit breaker halts feeds that deviate beyond a threshold |
| Stale price consumption | Staleness enforced at both submission and read time |
| EVM oracle data stranded off-chain | Accepts Chainlink ABI-encoded feed format directly |
| Decimal inconsistency across feeds | All prices normalized to 18 decimals for composability |
| Permissionless oracle poisoning | Updater whitelist restricts who can submit price data |

---

## How It Works

1. **Feed registration:** Admin registers each asset with per-feed configuration including staleness window, price bounds, circuit breaker threshold, and TWAP window size.
2. **Updater whitelisting:** Only admin-approved addresses can submit prices, preventing unauthorized oracle manipulation.
3. **Price submission:** Whitelisted updaters submit ABI-encoded `PriceFeedInput` structs. The contract decodes, validates, and stores the price if all checks pass.
4. **DeFi safety checks:** Each submission runs through staleness validation, sanity bound checks, decimal validation, and circuit breaker deviation checks before being committed.
5. **Consumption:** Other Soroban contracts call `get_price`, `get_twap`, `get_normalized_price`, or `get_price_abi` to consume validated price data.

---

## DeFi Safety Features

### Circuit Breaker
Each feed has a configurable `max_deviation_bps` (basis points). If a submitted price deviates from the previous price by more than this threshold, the circuit breaker trips and the feed is paused. The admin must manually reset it via `reset_circuit_breaker` after reviewing the anomaly.

### TWAP (Time-Weighted Average Price)
The contract maintains a rolling window of up to 10 price snapshots per asset. `get_twap` computes a time-weighted average across the window, giving each price a weight proportional to the duration it was active. This is resistant to single-block price manipulation.

### Price Normalization
All prices are normalized to 18 decimal places via `get_normalized_price`, regardless of the feed's native decimal precision. This enables direct composability with DeFi protocols that expect a standard precision.

### Historical Snapshots
The last N prices (configurable per feed via `twap_window`) are stored on-chain as `PriceSnapshot` records. These are accessible via `get_history` for on-chain auditability and TWAP computation.

### Per-Feed Staleness
Each feed has its own `max_staleness` window in seconds. Prices older than this threshold are rejected on both submission and read, ensuring consumers never receive outdated data.

---

## Contract API

### `initialize`
```rust
fn initialize(e: Env, admin: Address, max_staleness: u64)
```

Initializes the contract. Called once at deployment.

| Parameter | Type | Description |
|---|---|---|
| `admin` | `Address` | Address authorized to manage feeds and updaters |
| `max_staleness` | `u64` | Global default max price age in seconds |

---

### `register_feed`
```rust
fn register_feed(
    e: Env,
    caller: Address,
    asset: Bytes,
    max_staleness: u64,
    min_price: i128,
    max_price: i128,
    max_deviation_bps: u32,
    twap_window: u32,
) -> Result<(), Error>
```

Registers a new price feed asset. Admin only.

| Parameter | Type | Description |
|---|---|---|
| `asset` | `Bytes` | 32-byte asset identifier (e.g. ABI-encoded ticker) |
| `max_staleness` | `u64` | Max age in seconds before price is considered stale |
| `min_price` | `i128` | Sanity lower bound; `0` to disable |
| `max_price` | `i128` | Sanity upper bound; `0` to disable |
| `max_deviation_bps` | `u32` | Max allowed price change in basis points before circuit breaker trips |
| `twap_window` | `u32` | Number of snapshots to include in TWAP (capped at 10) |

---

### `set_updater`
```rust
fn set_updater(e: Env, caller: Address, updater: Address, allowed: bool) -> Result<(), Error>
```

Grants or revokes price submission rights for an address. Admin only.

---

### `set_feed_active`
```rust
fn set_feed_active(e: Env, caller: Address, asset: Bytes, active: bool) -> Result<(), Error>
```

Pauses or resumes a feed without deleting its configuration. Admin only.

---

### `reset_circuit_breaker`
```rust
fn reset_circuit_breaker(e: Env, caller: Address, asset: Bytes) -> Result<(), Error>
```

Resets a tripped circuit breaker, allowing new price submissions to resume. Admin only. Should only be called after manual review of the price anomaly.

---

### `submit`
```rust
fn submit(e: Env, caller: Address, input: Bytes) -> Result<(), Error>
```

Submits an ABI-encoded price update for a registered asset. Whitelisted updaters only.

| Parameter | Type | Description |
|---|---|---|
| `caller` | `Address` | Must be a whitelisted updater |
| `input` | `Bytes` | ABI-encoded `PriceFeedInput` struct |

**Execution steps:**
1. Decode ABI input
2. Verify feed is registered and active
3. Validate price against sanity bounds
4. Validate decimals (`<= 18`)
5. Validate timestamp against `max_staleness`
6. Check circuit breaker state
7. Check price deviation against `max_deviation_bps`; trip breaker if exceeded
8. Normalize price to 18 decimals
9. Append snapshot to rolling history window
10. Store updated `PriceEntry`

---

### `get_price`
```rust
fn get_price(e: Env, asset: Bytes) -> Result<PriceEntry, Error>
```

Returns the latest validated price entry. Fails if stale or circuit broken.

---

### `get_twap`
```rust
fn get_twap(e: Env, asset: Bytes) -> Result<i128, Error>
```

Returns the time-weighted average price across the stored snapshot window. Requires at least 2 snapshots. Falls back to simple average if all snapshots share the same timestamp.

---

### `get_normalized_price`
```rust
fn get_normalized_price(e: Env, asset: Bytes) -> Result<i128, Error>
```

Returns the latest price scaled to 18 decimal places.

---

### `get_price_abi`
```rust
fn get_price_abi(e: Env, asset: Bytes) -> Result<Bytes, Error>
```

Returns ABI-encoded `PriceFeedOutput` including price, TWAP, normalized value, and query timestamp. For consumption by EVM-compatible off-chain tooling.

---

### `get_history`
```rust
fn get_history(e: Env, asset: Bytes) -> Result<Vec<PriceSnapshot>, Error>
```

Returns the stored price snapshot history for an asset.

---

### `get_feed_config`
```rust
fn get_feed_config(e: Env, asset: Bytes) -> Result<FeedConfig, Error>
```

Returns the configuration for a registered feed.

---

### `is_fresh`
```rust
fn is_fresh(e: Env, asset: Bytes) -> bool
```

Returns `true` if the feed has a valid, non-stale, non-circuit-broken price. Intended for use as a pre-check by consuming contracts.

---

### `is_circuit_broken`
```rust
fn is_circuit_broken(e: Env, asset: Bytes) -> bool
```

Returns `true` if the circuit breaker has tripped for the given asset.

---

## Data Structures

### `PriceFeedInput` (ABI)

| Field | Solidity Type | Description |
|---|---|---|
| `asset` | `bytes32` | Asset identifier |
| `price` | `int256` | Raw price in feed's native decimals |
| `timestamp` | `uint256` | Unix timestamp of price observation |
| `decimals` | `uint8` | Decimal precision of the price |

### `PriceFeedOutput` (ABI)

| Field | Solidity Type | Description |
|---|---|---|
| `asset` | `bytes32` | Asset identifier |
| `price` | `int256` | Raw stored price |
| `timestamp` | `uint256` | Price observation timestamp |
| `decimals` | `uint8` | Feed decimal precision |
| `queried_at` | `uint256` | Ledger timestamp at query time |
| `twap` | `int256` | Time-weighted average price |
| `normalized` | `int256` | Price scaled to 18 decimals |

### `PriceEntry`

| Field | Type | Description |
|---|---|---|
| `asset` | `Bytes` | Asset identifier |
| `price` | `i128` | Raw stored price |
| `timestamp` | `u64` | Observation timestamp |
| `decimals` | `u32` | Feed decimal precision |
| `updated_at` | `u64` | Ledger timestamp when stored |
| `updater` | `Address` | Address that submitted this price |
| `normalized` | `i128` | Price scaled to 18 decimals |

### `FeedConfig`

| Field | Type | Description |
|---|---|---|
| `max_staleness` | `u64` | Max age in seconds |
| `min_price` | `i128` | Sanity lower bound |
| `max_price` | `i128` | Sanity upper bound |
| `active` | `bool` | Whether the feed accepts submissions |
| `max_deviation_bps` | `u32` | Circuit breaker threshold in basis points |
| `twap_window` | `u32` | Number of snapshots in TWAP window |

### `PriceSnapshot`

| Field | Type | Description |
|---|---|---|
| `price` | `i128` | Price at snapshot time |
| `timestamp` | `u64` | Observation timestamp |

---

## Error Reference

| Variant | Code | Description |
|---|---|---|
| `Unauthorized` | `1` | Caller is not admin or a whitelisted updater |
| `Decode` | `2` | ABI decoding of input failed |
| `StalePriceFeed` | `3` | Price timestamp exceeds `max_staleness` |
| `AssetNotFound` | `4` | No price stored for this asset |
| `InvalidPrice` | `5` | Price is zero, negative, or outside sanity bounds |
| `InvalidDecimals` | `6` | Decimal value exceeds 18 |
| `FeedAlreadyExists` | `7` | Asset feed has already been registered |
| `FeedNotRegistered` | `8` | Asset feed has not been registered or is inactive |
| `CircuitBreakerTripped` | `9` | Price deviated beyond threshold; feed is paused |
| `InsufficientHistory` | `10` | Fewer than 2 snapshots available for TWAP |

---

## Security Considerations

- **Updater whitelist.** Only admin-approved addresses can submit prices. Compromised updaters can be revoked via `set_updater` with `allowed: false`.
- **Circuit breaker is one-way.** Once tripped, only the admin can reset it. This prevents a compromised updater from self-clearing a breaker after a manipulated submission.
- **Per-feed sanity bounds.** Min and max price bounds provide a first layer of defense against grossly incorrect oracle data before deviation checks run.
- **Staleness enforced at both submit and read.** A price that passes submission can still become stale by the time it is read. Consuming contracts should handle `StalePriceFeed` errors gracefully.
- **TWAP manipulation resistance.** Because TWAP weights prices by time duration, a single flash-price submission has minimal effect on the average unless sustained across multiple blocks.
- **Normalization is lossy for high-decimal feeds.** Feeds with more than 18 decimals will have precision truncated during normalization. No feeds currently exceed 18 decimals in practice.

---

## Development

### Prerequisites

- Rust stable toolchain
- `wasm32-unknown-unknown` target: `rustup target add wasm32-unknown-unknown`
- [Stellar CLI](https://developers.stellar.org/docs/tools/developer-tools/cli/install-cli)

### Build
```bash
cargo build --target wasm32-unknown-unknown --release
```

Output: `target/wasm32-unknown-unknown/release/pricebridge.wasm`

### Test
```bash
cargo test
```

| Test | Description |
|---|---|
| `test_submit_and_get` | Valid price submission and retrieval |
| `test_normalized_price` | Price correctly scaled to 18 decimals |
| `test_twap_computed` | TWAP computed correctly across multiple submissions |
| `test_twap_insufficient_history` | Single submission returns `InsufficientHistory` |
| `test_circuit_breaker_trips_on_large_deviation` | >10% price move trips circuit breaker |
| `test_circuit_breaker_reset_by_admin` | Admin resets tripped circuit breaker |
| `test_price_history_stored` | Rolling snapshot history maintained correctly |
| `test_stale_rejected` | Price with old timestamp rejected on submit |
| `test_abi_output_includes_twap_and_normalized` | ABI output contains enriched fields |

---

## Deployment

### Prerequisites

- [Stellar CLI](https://developers.stellar.org/docs/tools/developer-tools/cli/install-cli) installed
- Rust `wasm32-unknown-unknown` target: `rustup target add wasm32-unknown-unknown`
- A funded Stellar testnet account

### Step 1 — Add Testnet Network
```bash
soroban network add \
  --rpc-url https://soroban-testnet.stellar.org:443 \
  --network-passphrase "Test SDF Network ; September 2015" \
  testnet
```

### Step 2 — Generate and Fund a Keypair
```bash
stellar keys generate alice --network testnet --fund
stellar keys address alice
```

### Step 3 — Build the Contract
```bash
cargo build --release --target wasm32-unknown-unknown
```

### Step 4 — Deploy to Testnet
```bash
stellar contract deploy \
  --wasm target/wasm32-unknown-unknown/release/pricebridge.wasm \
  --source-account alice \
  --network testnet
```

On success you will receive a contract ID:
```
CDNGJSUYHQRJYHYGLMFFBOG6VLISVEA2FFNKKLFU3DPT7LB6R3SSZXGZ
```

Save this — it is required for all subsequent invocations.

### Step 5 — Initialize the Contract
```bash
stellar contract invoke \
  --id <CONTRACT_ID> \
  --source-account alice \
  --network testnet \
  -- initialize \
  --admin $(stellar keys address alice) \
  --max_staleness 300
```

### Step 6 — Whitelist an Updater
```bash
stellar contract invoke \
  --id <CONTRACT_ID> \
  --source-account alice \
  --network testnet \
  -- set_updater \
  --caller $(stellar keys address alice) \
  --updater $(stellar keys address alice) \
  --allowed true
```

### Step 7 — Register a Price Feed
```bash
stellar contract invoke \
  --id <CONTRACT_ID> \
  --source-account alice \
  --network testnet \
  -- register_feed \
  --caller $(stellar keys address alice) \
  --asset <ASSET_BYTES_HEX> \
  --max_staleness 300 \
  --min_price 1000000 \
  --max_price 1000000000 \
  --max_deviation_bps 1000 \
  --twap_window 5
```

### Step 8 — Submit a Price
```bash
stellar contract invoke \
  --id <CONTRACT_ID> \
  --source-account alice \
  --network testnet \
  -- submit \
  --caller $(stellar keys address alice) \
  --input <ABI_ENCODED_PRICE_FEED_INPUT_HEX>
```

### Step 9 — Query a Price
```bash
stellar contract invoke \
  --id <CONTRACT_ID> \
  --source-account alice \
  --network testnet \
  -- get_price \
  --asset <ASSET_BYTES_HEX>
```

### Step 10 — Check Feed Health
```bash
# Check if price is fresh
stellar contract invoke \
  --id <CONTRACT_ID> \
  --source-account alice \
  --network testnet \
  -- is_fresh \
  --asset <ASSET_BYTES_HEX>

# Check if circuit breaker has tripped
stellar contract invoke \
  --id <CONTRACT_ID> \
  --source-account alice \
  --network testnet \
  -- is_circuit_broken \
  --asset <ASSET_BYTES_HEX>
```

### Deployed Contract (Testnet)

| Field | Value |
|---|---|
| Network | Stellar Testnet |
| Contract ID | `CDNGJSUYHQRJYHYGLMFFBOG6VLISVEA2FFNKKLFU3DPT7LB6R3SSZXGZ` |
| Explorer | [View on Stellar Expert](https://stellar.expert/explorer/testnet/contract/CDNGJSUYHQRJYHYGLMFFBOG6VLISVEA2FFNKKLFU3DPT7LB6R3SSZXGZ) |
| Stellar Lab | [View on Stellar Lab](https://lab.stellar.org/r/testnet/contract/CDNGJSUYHQRJYHYGLMFFBOG6VLISVEA2FFNKKLFU3DPT7LB6R3SSZXGZ) |

---

## Project Structure
```
.
├── src/
│   ├── lib.rs       # Contract logic, DeFi features, ABI structs
│   └── test.rs      # Unit tests
├── Cargo.toml       # Dependencies and release profiles
└── README.md
```
