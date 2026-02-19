# dexter-kupo-rs API Reference

## Overview

A Rust library for querying Cardano DEX liquidity pools via Kupo. Supports 10+ DEXes.

## Quick Reference

### Initialize Kupo
```rust
use dexter_kupo_rs::KupoApi;
let kupo = KupoApi::new("http://157.180.117.47:1444");
```

### Query AMM Pools (Minswap, SundaeSwap, WingRiders, etc.)
```rust
use dexter_kupo_rs::dex::{BaseDex, MinswapV2};

let dex = MinswapV2::new(kupo);
let pools = dex.liquidity_pools_from_token("lovelace", "<token_id>").await?;
// Returns Vec<LiquidityPool>
```

### Query VyFinance (with optional caching)
```rust
use dexter_kupo_rs::dex::vyfinance::VyFinance;

let dex = VyFinance::new(kupo);

// Without cache (fetches from API each time)
let pools = dex.liquidity_pools_from_token("lovelace", "<token_id>").await?;

// With cache (faster - see Cache section below)
let pools = dex.liquidity_pools_from_token_cached("lovelace", "<token_id>", Some(&cache)).await?;
```

### Query MinswapStable (Curve-style)
```rust
use dexter_kupo_rs::dex::MinswapStable;

let stable = MinswapStable::new(kupo);
let pool = stable.get_pool(pool_addr, asset_a, asset_b, decimals_a, decimals_b).await?;
// Returns StablePool
```

### Query ChadSwap Order Book
```rust
use dexter_kupo_rs::dex::ChadSwap;

// ChadSwap fetches directly from api.chadswap.com — no Kupo required
let chadswap = ChadSwap::new();

// Single token
let book = chadswap.get_orders_by_token("<token_id>").await?;
// Returns OrderBook { token_id, buy_orders, sell_orders }

// All tokens at once
let books = chadswap.get_all_order_books().await?;
// Returns HashMap<String, OrderBook>
```

### Query VyFi Bar Rate
```rust
use dexter_kupo_rs::dex::vyfi_bar::VyfiBar;

let vyfibar = VyfiBar::new(kupo);
let rate = vyfibar.get_rate("<pool_identifier>").await?;
// Returns Rate { pool_identifier, base_asset, derived_asset }
```

## DEX Classes

| Class | File | Use For | Caching |
|-------|------|---------|---------|
| `MinswapV1` | `dex/minswap_v1.rs` | Minswap V1 pools | N/A |
| `MinswapV2` | `dex/minswap_v2.rs` | Minswap V2 pools | N/A |
| `MinswapStable` | `dex/minswap_stable.rs` | Minswap stable (Curve-style) | N/A |
| `SundaeSwapV1` | `dex/sundaeswap_v1.rs` | SundaeSwap V1 pools | N/A |
| `SundaeSwapV3` | `dex/sundaeswap_v3.rs` | SundaeSwap V3 pools | N/A |
| `WingRiders` | `dex/wingriders.rs` | WingRiders V1 pools | N/A |
| `WingRidersV2` | `dex/wingriders_v2.rs` | WingRiders V2 pools | N/A |
| `CSwap` | `dex/cswap.rs` | CSwap pools | N/A |
| `VyFinance` | `dex/vyfinance.rs` | VyFinance pools | **Supported** |
| `ChadSwap` | `dex/chadswap.rs` | ChadSwap order book (via API, no Kupo) | N/A |
| `VyfiBar` | `dex/vyfi_bar.rs` | VyFi staking rates | N/A |

## Models

### LiquidityPool
```rust
pub struct LiquidityPool {
    pub dex_identifier: String,
    pub asset_a: Token,
    pub asset_b: Token,
    pub reserve_a: u64,
    pub reserve_b: u64,
    pub address: String,
    pub pool_id: String,
    pub pool_fee_percent: f64,
    pub total_lp_tokens: u64,
}

impl LiquidityPool {
    pub fn pair(&self) -> String;        // "ADA/MELD"
    pub fn price(&self) -> f64;          // reserve_b / reserve_a
    pub fn uuid(&self) -> String;
}
```

### StablePool
```rust
pub struct StablePool {
    pub dex_identifier: String,
    pub asset_a: Token,
    pub asset_b: Token,
    pub reserve_a: u64,
    pub reserve_b: u64,
    pub pool_fee_percent: f64,
    pub amplification_coefficient: u64,  // "A" parameter
    pub total_liquidity: u64,            // "D" invariant
}
```

### Order (ChadSwap)
```rust
pub struct Order {
    pub asset: Token,
    pub amount: u64,           // RemainingAmount
    pub price: u64,            // UnitPrice
    pub price_denominator: u64,
    pub is_buy: bool,
}
```

### OrderBook (ChadSwap)
```rust
pub struct OrderBook {
    pub token_id: String,
    pub buy_orders: Vec<Order>,
    pub sell_orders: Vec<Order>,
}
```

### Rate (VyFi Bar)
```rust
pub struct Rate {
    pub pool_identifier: String,
    pub base_asset: u64,
    pub derived_asset: u64,
}
```

### Token
```rust
pub enum Token {
    Lovelace,
    Asset(Asset),
}

pub struct Asset {
    pub policy_id: String,     // 56 hex chars
    pub name_hex: String,     // hex encoded
    pub decimals: u8,
}
```

### Cache Helpers (for VyFinance)

```rust
use dexter_kupo_rs::cache::{load_from_file, save_to_file};

// Save/Load JSON cache files
save_to_file(&data, "cache.json")?;
let data: MyType = load_from_file("cache.json")?;
```

### Utxo
```rust
pub struct Utxo {
    pub address: String,
    pub tx_hash: String,
    pub tx_index: u32,
    pub output_index: u32,
    pub amount: Vec<Unit>,
    pub data_hash: Option<String>,
}

pub struct Unit {
    pub unit: String,    // "lovelace" or "<policy><namehex>"
    pub quantity: String,
}
```

## Token ID Format

Token IDs are **concatenated policy + name** (no dot):
```
# Policy: f13ac4d66b3ee19a6aa0f2a22298737bd907cc95121662fc971b5275
# Name:   534c4545444b455259 (MELDKERRY in hex)
# TokenID: f13ac4d66b3ee19a6aa0f2a22298737bd907cc95121662fc971b5275535452494b45

Use "lovelace" for ADA.
```

## BaseDex Trait

```rust
#[async_trait]
pub trait BaseDex: Send + Sync {
    fn identifier(&self) -> &str;
    fn pool_address(&self) -> &str;
    fn lp_token_policy_id(&self) -> &str;
    fn kupo(&self) -> &KupoApi;

    async fn all_liquidity_pool_utxos(&self) -> Result<Vec<Utxo>>;
    
    async fn liquidity_pool_from_utxo(&self, utxo: &Utxo, pool_id: &str) -> Result<Option<LiquidityPool>>;
    
    async fn liquidity_pool_from_utxo_extend(&self, utxo: &Utxo, pool_id: &str) -> Result<Option<LiquidityPool>>;
    
    async fn liquidity_pool_from_pool_id(&self, pool_id: &str) -> Result<Option<LiquidityPool>>;
    
    async fn liquidity_pools_from_token(&self, token_b: &str, token_a: &str) -> Result<Vec<LiquidityPool>>;
}
```

## VyFinance Caching

VyFinance uses an external API for pool metadata. Cache the metadata and pass it to avoid repeated API calls:

```rust
use dexter_kupo_rs::cache::{load_from_file, save_to_file};
use dexter_kupo_rs::dex::vyfinance::{VyFinance, VyFinanceCache};

// Build cache once
let pool_data = dex.fetch_all_pool_data().await?;
let cache = VyFinance::structure_pool_data(pool_data);
save_to_file(&cache, "cache.json")?;

// Use cache
let cache = load_from_file::<VyFinanceCache>("cache.json")?;
let pools = dex.liquidity_pools_from_token_cached("lovelace", "token", Some(&cache)).await?;
```

CLI: `cargo run --release -- --dex vyfinance --cache cache.json lovelace <token>`

## Error Handling

All async methods return `Result<T, anyhow::Error>`. Use `?` for propagation.

## CLI Commands

```bash
# Export pools
cargo run --release -- --dex minswap_v2

# Query pair
cargo run --release -- --dex minswap_v1 lovelace <token_id>

# VyFinance (use --cache for faster repeated queries)
cargo run --release -- --dex vyfinance --cache cache.json lovelace <token_id>

# Stable pool
cargo run --release -- --dex minswap_stable <pool_addr> <asset_a> <asset_b> 6 6

# Order book for a specific token
cargo run --release -- --dex chadswap <token_id>

# All order books (all tokens)
cargo run --release -- --dex chadswap_all

# VyFi rate
cargo run --release -- --vyfi-bar <pool_identifier>
```
