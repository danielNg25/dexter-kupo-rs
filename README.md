# dexter-kupo-rs

A Rust library for interacting with multiple Cardano DEX protocols. Queries liquidity pools and facilitates token swaps via Kupo (UTXO indexer).

## Supported DEXes

| DEX | Type | Model | CLI Flag |
|-----|------|-------|----------|
| MinswapV1 | AMM | LiquidityPool | `minswap_v1` |
| MinswapV2 | AMM | LiquidityPool | `minswap_v2` |
| MinswapStable | Stable AMM | StablePool | `minswap_stable` |
| SundaeSwapV1 | AMM | LiquidityPool | `sundaeswap_v1` |
| SundaeSwapV3 | AMM (Concentrated) | LiquidityPool | `sundaeswap_v3` |
| WingRiders | AMM | LiquidityPool | `wingriders` |
| WingRidersV2 | AMM | LiquidityPool (+ stable detection) | `wingriders_v2` |
| CSwap | AMM | LiquidityPool | `cswap` |
| VyFinance | AMM | LiquidityPool | `vyfinance` |
| VyFi Bar | Rate Provider | Rate | `--vyfi-bar` |
| ChadSwap | Order Book | Order/OrderBook | `chadswap` |

## Installation

```toml
# Cargo.toml
[dependencies]
dexter-kupo-rs = "0.1"
```

## Quick Start

```rust
use dexter_kupo_rs::{KupoApi, dex::{MinswapV2, BaseDex}};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let kupo = KupoApi::new("http://157.180.117.47:1444");
    let dex = MinswapV2::new(kupo);

    // Query pools for ADA / MELD
    let pools = dex.liquidity_pools_from_token(
        "lovelace",
        "f13ac4d66b3ee19a6aa0f2a22298737bd907cc95121662fc971b5275535452494b45"
    ).await?;

    for pool in pools {
        println!("Pool: {} — Reserves: {} / {}",
            pool.pool_id,
            pool.reserve_a,
            pool.reserve_b
        );
    }

    Ok(())
}
```

## CLI Usage

```bash
# Build
cargo build --release

# Export all pools for a DEX to pools_rs.json
cargo run --release -- --dex minswap_v2

# Query specific token pair
cargo run --release -- --dex minswap_v1 lovelace f13ac4d66b3ee19a6aa0f2a22298737bd907cc95121662fc971b5275535452494b45

# Query MinswapStable (requires pool address + assets)
cargo run --release -- --dex minswap_stable addr1wx4w03kq5tfhaad2fmglefgejj0anajcsvvg88w96lrmylc7mx5rm <asset_a> <asset_b> 6 6

# Query ChadSwap order book
cargo run --release -- --dex chadswap <token_id>

# Query VyFi Bar rate
cargo run --release -- --vyfi-bar <pool_identifier>
```

## Token Identifiers

Token identifiers are the concatenation of policy ID and hex-encoded asset name (no separator):

```
# Example: MELD token
policy_id = "f13ac4d66b3ee19a6aa0f2a22298737bd907cc95121662fc971b5275"
asset_name = "534c4545444b455259" (hex for "MELDKERRY")
token_id   = "f13ac4d66b3ee19a6aa0f2a22298737bd907cc95121662fc971b5275535452494b45"
```

Use `lovelace` for ADA.

## Library API Implementations

```rust

### DEX
// All DEXes implement BaseDex trait
use dexter_kupo_rs::{KupoApi, dex::{BaseDex, MinswapV2, SundaeSwapV1, WingRiders, VyFinance, ChadSwap, MinswapStable, VyfiBar}};

// AMM DEXes
let dex = MinswapV2::new(kupo.clone());
let pools = dex.liquidity_pools_from_token(token_a, token_b).await?;

// Order Book DEX
let chadswap = ChadSwap::new(kupo.clone());
let orderbook = chadswap.get_orders_by_token(token_id).await?;

// Stable Pool
let stable = MinswapStable::new(kupo.clone());
let pool = stable.get_pool(pool_addr, asset_a, asset_b, decimals_a, decimals_b).await?;

// VyFi Bar Rate
let vyfibar = VyfiBar::new(kupo.clone());
let rate = vyfibar.get_rate(pool_identifier).await?;
```

### Models

```rust
use dexter_kupo_rs::{LiquidityPool, StablePool, Order, OrderBook, Rate, Token, Utxo};

// LiquidityPool - for AMM DEXes
let reserve_a = pool.reserve_a;
let reserve_b = pool.reserve_b;
let fee = pool.pool_fee_percent;
let price = pool.price();

// StablePool - for MinswapStable
let amp_coeff = pool.amplification_coefficient;
let total_liq = pool.total_liquidity;

// Order - for ChadSwap
let price = order.price;
let amount = order.amount;
let is_buy = order.is_buy;

// Rate - for VyFi Bar
let base = rate.base_asset;
let derived = rate.derived_asset;
```

### KupoApi

```rust
use dexter_kupo_rs::KupoApi;

let kupo = KupoApi::new("http://157.180.117.47:1444");

// Query UTXOs by address/pattern
let utxos = kupo.get("addr1xxx", true).await?;

// Fetch datum by hash
let datum = kupo.datum("abc123...").await?;
```

## Architecture

- **`dex/`** — DEX implementations (each DEX is a module)
- **`models/`** — Data structures (LiquidityPool, Token, Utxo, etc.)
- **`kupo.rs`** — Kupo API client
- **`utils/`** — Retry logic, helpers

## License

MIT
