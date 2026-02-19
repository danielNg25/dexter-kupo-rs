//! # dexter-kupo-rs
//!
//! A Rust library for interacting with multiple Cardano DEX protocols.
//! Queries liquidity pools and facilitates token swaps via Kupo (UTXO indexer) and Blockfrost.
//!
//! ## Supported DEXes
//!
//! | DEX | Type | Model | Discovery |
//! |-----|------|-------|-----------|
//! | MinswapV1 | AMM | LiquidityPool | Validity asset |
//! | MinswapV2 | AMM | LiquidityPool | Script address |
//! | MinswapStable | Stable AMM | StablePool | Direct address |
//! | SundaeSwapV1 | AMM | LiquidityPool | LP token policy |
//! | SundaeSwapV3 | AMM | LiquidityPool | Two addresses |
//! | WingRiders | AMM | LiquidityPool | Validity asset |
//! | WingRidersV2 | AMM | LiquidityPool | Validity asset + stable detection |
//! | CSwap | AMM | LiquidityPool | LP token name "63" |
//! | VyFinance | AMM | LiquidityPool | API + NFT |
//! | ChadSwap | Order Book | Order/OrderBook | Contract addresses |
//!
//! ## Quick Start
//!
//! ```rust
//! use dexter_kupo_rs::{KupoApi, dex::{MinswapV2, BaseDex}};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let kupo = KupoApi::new("http://157.180.117.47:1444");
//!     let dex = MinswapV2::new(kupo);
//!
//!     // Query pools for a token pair
//!     let pools = dex.liquidity_pools_from_token(
//!         "lovelace",
//!         "f13ac4d66b3ee19a6aa0f2a22298737bd907cc95121662fc971b5275535452494b45"
//!     ).await?;
//!
//!     for pool in pools {
//!         println!("Pool: {} / {} = {}",
//!             pool.pair(),
//!             pool.reserve_a,
//!             pool.reserve_b
//!         );
//!     }
//!
//!     Ok(())
//! }
//! ```
//!
//! ## CLI Usage
//!
//! The library includes a binary for convenient CLI usage:
//!
//! ```bash
//! # Export all pools for a DEX
//! cargo run --release -- --dex minswap_v2
//!
//! # Query specific token pair
//! cargo run --release -- --dex minswap_v1 lovelace f13ac4d66b3ee19a6aa0f2a22298737bd907cc95121662fc971b5275535452494b45
//!
//! # Query MinswapStable (requires pool address + assets)
//! cargo run --release -- --dex minswap_stable <pool_addr> <asset_a> <asset_b> 6 6
//!
//! # Query ChadSwap order book
//! cargo run --release -- --dex chadswap <token_id>
//!
//! # Query VyFi Bar rate
//! cargo run --release -- --vyfi-bar <pool_identifier>
//! ```

pub mod cache;
pub mod dex;
pub mod kupo;
pub mod models;
pub mod utils;

pub use cache::{load_from_file, save_to_file};
pub use dex::BaseDex;
pub use dex::vyfinance::{VyFinanceCache, VyFinancePoolData};
pub use kupo::KupoApi;
pub use models::{Asset, LiquidityPool, Order, OrderBook, StablePool, Token, Utxo};
