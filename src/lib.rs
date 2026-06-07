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
//! | ChadSwap | Order Book | Order/OrderBook | Direct API (no Kupo) |
//!
//! ## Quick Start
//!
//! ```no_run
//! use dexter_kupo_rs::{KupoApi, dex::{minswap_v2::MinswapV2, BaseDex}};
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
//! # Query ChadSwap order book (by token)
//! cargo run --release -- --dex chadswap <token_id>
//!
//! # Query all ChadSwap order books
//! cargo run --release -- --dex chadswap_all
//!
//! # Query VyFi Bar rate
//! cargo run --release -- --vyfi-bar <pool_identifier>
//! ```
//!
//! ## Building a Minswap V2 limit order
//!
//! ```no_run
//! use dexter_kupo_rs::{KupoApi, SwapRequest, Token};
//! use dexter_kupo_rs::dex::minswap_v2::MinswapV2;
//!
//! # async fn doc(pool: dexter_kupo_rs::LiquidityPool) -> anyhow::Result<()> {
//! let dex = MinswapV2::new(KupoApi::new("http://localhost:1442"));
//! let pays = SwapRequest::new(&dex)
//!     .for_pool(pool)
//!     .with_swap_in_token(Token::Lovelace)
//!     .with_swap_in_amount(2_000_000_000)
//!     .with_minimum_receive(341_880_341)
//!     .with_sender_address("addr1...")?
//!     .build()?;
//! // Hand `pays` to your tx-building layer (Phase 2).
//! # Ok(()) }
//! ```

pub mod address;
pub mod cache;
pub mod dex;
pub mod kupo;
pub mod models;
pub mod plutus;
pub mod requests;
pub mod utils;

pub use address::{decode_base_address, script_and_stake_to_base_address, WalletAddress};
pub use cache::{load_from_file, save_to_file};
pub use dex::{BaseDex, DexSwap};
pub use dex::vyfinance::{VyFinanceCache, VyFinancePoolData};
pub use kupo::KupoApi;
pub use models::{Asset, LiquidityPool, Order, OrderBook, StablePool, Token, Utxo};
pub use plutus::PlutusData;
pub use requests::{
    AddressType, AssetAmount, CancelSwapRequest, OrderKind, PayToAddress, PlutusScript,
    PlutusVersion, SpendUtxo, SwapFee, SwapParams, SwapRequest, UpdateSwapRequest,
};
