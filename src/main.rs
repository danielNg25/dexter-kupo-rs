mod cache;
mod dex;
mod kupo;
mod models;
mod utils;

use crate::models::asset::token_identifier;
use dex::chadswap::ChadSwap;
use dex::cswap::CSwap;
use dex::minswap_v1::MinswapV1;
use dex::minswap_v2::MinswapV2;
use dex::minswap_stable::MinswapStable;
use dex::sundaeswap_v1::SundaeSwapV1;
use dex::sundaeswap_v3::SundaeSwapV3;
use dex::vyfinance::{VyFinance, VyFinanceCache};
use dex::vyfi_bar::VyfiBar;
use dex::wingriders::WingRiders;
use dex::wingriders_v2::WingRidersV2;
use dex::BaseDex;
use kupo::KupoApi;
use models::LiquidityPool;
use serde::Serialize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Semaphore;

const CONCURRENCY: usize = 5;
const KUPO_URL: &str = "http://157.180.117.47:1444";

#[derive(Serialize)]
struct PoolExport {
    dex: String,
    pool_id: String,
    asset_a: String,
    asset_b: String,
    reserve_a: String,
    reserve_b: String,
    pool_fee_percent: f64,
    total_lp_tokens: String,
    tx_hash: String,
}

#[derive(Serialize)]
struct StablePoolExport {
    dex: String,
    pool_id: String,
    asset_a: String,
    asset_b: String,
    reserve_a: String,
    reserve_b: String,
    pool_fee_percent: f64,
    amplification_coefficient: String,
    total_liquidity: String,
}

fn pool_to_export(pool: &LiquidityPool, tx_hash: &str) -> PoolExport {
    PoolExport {
        dex: pool.dex_identifier.clone(),
        pool_id: pool.pool_id.clone(),
        asset_a: token_identifier(&pool.asset_a),
        asset_b: token_identifier(&pool.asset_b),
        reserve_a: pool.reserve_a.to_string(),
        reserve_b: pool.reserve_b.to_string(),
        pool_fee_percent: pool.pool_fee_percent,
        total_lp_tokens: pool.total_lp_tokens.to_string(),
        tx_hash: tx_hash.to_string(),
    }
}

fn print_usage(bin: &str) {
    eprintln!("Usage:");
    eprintln!(
        "  {} [--dex <dex_name>] [--cache <path>] [asset_a asset_b]",
        bin
    );
    eprintln!(
        "  {} --vyfi-bar <pool_identifier>",
        bin
    );
    eprintln!();
    eprintln!("  No args          → export all pools to pools_rs.json");
    eprintln!("  asset_a asset_b  → print matching pools to stdout");
    eprintln!("  --dex            → choose DEX (default: minswap_v2)");
    eprintln!("  --cache          → path to vyfinance cache JSON file");
    eprintln!("  --vyfi-bar       → fetch VyFi Bar rate for a pool identifier");
    eprintln!();
    eprintln!("  Available DEXes:");
    eprintln!("    minswap_v1, minswap_v2");
    eprintln!("    sundaeswap_v1, sundaeswap_v3");
    eprintln!("    wingriders, wingriders_v2");
    eprintln!("    cswap");
    eprintln!("    vyfinance");
    eprintln!("    minswap_stable  (requires: pool_address asset_a asset_b [decimals_a] [decimals_b])");
    eprintln!("    chadswap          (requires: token_id — order book query by token)");
    eprintln!("    chadswap_all      (no args — fetch all order books via ChadSwap API)");
    eprintln!();
    eprintln!("  Use 'lovelace' for ADA.");
    eprintln!("  Examples:");
    eprintln!("    cargo run --release -- --dex minswap_v1 lovelace f13ac4d66b3ee19a6aa0f2a22298737bd907cc95121662fc971b5275535452494b45");
    eprintln!("    cargo run --release -- --dex vyfinance --cache vyfi_cache.json lovelace f13ac4d66b3ee19a6aa0f2a22298737bd907cc95121662fc971b5275535452494b45");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let raw_args: Vec<String> = std::env::args().collect();

    let mut dex_name = "minswap_v2".to_string();
    let mut cache_path: Option<String> = None;
    let mut vyfi_bar_id: Option<String> = None;
    let mut assets: Vec<String> = Vec::new();
    let mut i = 1;
    while i < raw_args.len() {
        if raw_args[i] == "--dex" {
            i += 1;
            if i >= raw_args.len() {
                eprintln!("--dex requires a value");
                std::process::exit(1);
            }
            dex_name = raw_args[i].clone();
        } else if raw_args[i] == "--cache" {
            i += 1;
            if i >= raw_args.len() {
                eprintln!("--cache requires a path");
                std::process::exit(1);
            }
            cache_path = Some(raw_args[i].clone());
        } else if raw_args[i] == "--vyfi-bar" {
            i += 1;
            if i >= raw_args.len() {
                eprintln!("--vyfi-bar requires a pool identifier");
                std::process::exit(1);
            }
            vyfi_bar_id = Some(raw_args[i].clone());
        } else {
            assets.push(raw_args[i].clone());
        }
        i += 1;
    }

    let kupo = KupoApi::new(KUPO_URL);

    if let Some(pool_id) = vyfi_bar_id {
        fetch_vyfi_bar_rate(VyfiBar::new(kupo), &pool_id).await?;
        return Ok(());
    }

    match dex_name.as_str() {
        "minswap_v1" => run(MinswapV1::new(kupo), &assets, &raw_args[0]).await?,
        "minswap_v2" => run(MinswapV2::new(kupo), &assets, &raw_args[0]).await?,
        "sundaeswap_v1" => run(SundaeSwapV1::new(kupo), &assets, &raw_args[0]).await?,
        "sundaeswap_v3" => run(SundaeSwapV3::new(kupo), &assets, &raw_args[0]).await?,
        "wingriders" => run(WingRiders::new(kupo), &assets, &raw_args[0]).await?,
        "wingriders_v2" => run(WingRidersV2::new(kupo), &assets, &raw_args[0]).await?,
        "cswap" => run(CSwap::new(kupo), &assets, &raw_args[0]).await?,
        "vyfinance" => {
            let dex = VyFinance::new(kupo);
            if assets.len() == 2 {
                let cache = if let Some(path) = &cache_path {
                    match cache::load_from_file::<VyFinanceCache>(path) {
                        Ok(c) => Some(c),
                        Err(_) => {
                            eprintln!("Cache file not found or invalid. Fetching from API...");
                            let pool_data = dex.fetch_all_pool_data().await?;
                            let c = VyFinance::structure_pool_data(pool_data);
                            let _ = cache::save_to_file(&c, path);
                            Some(c)
                        }
                    }
                } else {
                    None
                };
                
                eprintln!("Querying pools for {} / {}...", assets[0], assets[1]);
                let pools = dex.liquidity_pools_from_token_cached(&assets[1], &assets[0], cache.as_ref()).await?;
                if pools.is_empty() {
                    eprintln!("No pools found.");
                } else {
                    eprintln!("Found {} pool(s).", pools.len());
                    let exports: Vec<PoolExport> = pools.iter().map(|p| pool_to_export(p, "")).collect();
                    println!("{}", serde_json::to_string_pretty(&exports)?);
                }
            } else if assets.is_empty() {
                export_all_vyfinance(dex).await?;
            } else {
                print_usage(&raw_args[0]);
                std::process::exit(1);
            }
        }
        "chadswap" => {
            if assets.len() != 1 {
                eprintln!("chadswap requires exactly 1 positional arg: <token_id>");
                print_usage(&raw_args[0]);
                std::process::exit(1);
            }
            fetch_chadswap_orders(ChadSwap::new(), &assets[0]).await?;
        }
        "chadswap_all" => {
            let dex = ChadSwap::new();
            let books = dex.get_all_order_books().await?;
            eprintln!("[chadswap] found order books for {} tokens", books.len());
            println!("{}", serde_json::to_string_pretty(&books)?);
        }
        "minswap_stable" => {
            if assets.len() < 3 {
                eprintln!("minswap_stable requires at least 3 positional args: <pool_address> <asset_a> <asset_b>");
                print_usage(&raw_args[0]);
                std::process::exit(1);
            }
            let pool_address = &assets[0];
            let asset_a = &assets[1];
            let asset_b = &assets[2];
            let decimals_a: u8 = assets.get(3).and_then(|s| s.parse().ok()).unwrap_or(6);
            let decimals_b: u8 = assets.get(4).and_then(|s| s.parse().ok()).unwrap_or(6);
            fetch_stable_pool(
                MinswapStable::new(kupo),
                pool_address,
                asset_a,
                asset_b,
                decimals_a,
                decimals_b,
            )
            .await?;
        }
        other => {
            eprintln!(
                "Unknown dex: '{}'. Run with no args to see available DEXes.",
                other
            );
            print_usage(&raw_args[0]);
            std::process::exit(1);
        }
    }

    Ok(())
}

async fn run<D: BaseDex + Send + Sync + 'static>(
    dex: D,
    assets: &[String],
    bin: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    match assets.len() {
        0 => export_all(dex).await,
        2 => fetch_pair(dex, &assets[0], &assets[1]).await,
        _ => {
            print_usage(bin);
            std::process::exit(1);
        }
    }
}

async fn fetch_pair<D: BaseDex>(
    dex: D,
    asset_a: &str,
    asset_b: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("Querying pools for {} / {}...", asset_a, asset_b);
    let pools = dex.liquidity_pools_from_token(asset_b, asset_a).await?;

    if pools.is_empty() {
        eprintln!("No pools found.");
    } else {
        eprintln!("Found {} pool(s).", pools.len());
        let exports: Vec<PoolExport> = pools.iter().map(|p| pool_to_export(p, "")).collect();
        println!("{}", serde_json::to_string_pretty(&exports)?);
    }

    Ok(())
}

async fn export_all<D: BaseDex + Send + Sync + 'static>(
    dex: D,
) -> Result<(), Box<dyn std::error::Error>> {
    let dex = Arc::new(dex);

    eprintln!("Fetching all pool UTXOs...");
    let utxos = Arc::new(dex.all_liquidity_pool_utxos().await?);
    let total = utxos.len();
    eprintln!("Found {} UTXOs", total);

    let semaphore = Arc::new(Semaphore::new(CONCURRENCY));
    let done = Arc::new(AtomicUsize::new(0));
    let skipped = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::with_capacity(total);

    for i in 0..total {
        let dex = Arc::clone(&dex);
        let utxos = Arc::clone(&utxos);
        let sem = Arc::clone(&semaphore);
        let done = Arc::clone(&done);
        let skipped = Arc::clone(&skipped);

        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let utxo = &utxos[i];

            let result: Option<PoolExport> = async {
                let base = match dex.liquidity_pool_from_utxo(utxo, "").await {
                    Ok(Some(p)) => p,
                    Ok(None) => return None,
                    Err(e) => {
                        eprintln!("\n[error] utxo {} base: {}", utxo.tx_hash, e);
                        return None;
                    }
                };
                match dex
                    .liquidity_pool_from_utxo_extend(utxo, &base.pool_id)
                    .await
                {
                    Ok(Some(pool)) => Some(pool_to_export(&pool, &utxo.tx_hash)),
                    Ok(None) => None,
                    Err(e) => {
                        eprintln!("\n[error] utxo {} extend: {}", utxo.tx_hash, e);
                        None
                    }
                }
            }
            .await;

            let d = done.fetch_add(1, Ordering::Relaxed) + 1;
            if result.is_none() {
                skipped.fetch_add(1, Ordering::Relaxed);
            }
            if d % 10 == 0 || d == total {
                let s = skipped.load(Ordering::Relaxed);
                eprint!("\r[{}/{}] pools={} skipped={}   ", d, total, d - s, s);
            }
            result
        });

        handles.push(handle);
    }

    let mut pools: Vec<PoolExport> = Vec::new();
    for handle in handles {
        if let Ok(Some(pool)) = handle.await {
            pools.push(pool);
        }
    }
    eprintln!();

    pools.sort_by(|a, b| a.pool_id.cmp(&b.pool_id));
    let json = serde_json::to_string_pretty(&pools)?;
    std::fs::write("pools_rs.json", &json)?;
    eprintln!(
        "Exported {} pools to pools_rs.json (skipped {})",
        pools.len(),
        total - pools.len()
    );

    Ok(())
}

async fn fetch_chadswap_orders(
    dex: ChadSwap,
    token_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("[chadswap] fetching orders for token: {}", token_id);
    let book = dex.get_orders_by_token(token_id).await?;
    eprintln!(
        "[chadswap] found {} buy orders, {} sell orders",
        book.buy_orders.len(),
        book.sell_orders.len()
    );
    println!("{}", serde_json::to_string_pretty(&book)?);
    Ok(())
}

async fn fetch_stable_pool(
    dex: MinswapStable,
    pool_address: &str,
    asset_a: &str,
    asset_b: &str,
    decimals_a: u8,
    decimals_b: u8,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("[minswap_stable] fetching pool at: {}", pool_address);
    let pool = dex
        .get_pool(pool_address, asset_a, asset_b, decimals_a, decimals_b)
        .await?;

    let export = StablePoolExport {
        dex: pool.dex_identifier.clone(),
        pool_id: pool.pool_id.clone(),
        asset_a: token_identifier(&pool.asset_a),
        asset_b: token_identifier(&pool.asset_b),
        reserve_a: pool.reserve_a.to_string(),
        reserve_b: pool.reserve_b.to_string(),
        pool_fee_percent: pool.pool_fee_percent,
        amplification_coefficient: pool.amplification_coefficient.to_string(),
        total_liquidity: pool.total_liquidity.to_string(),
    };

    println!("{}", serde_json::to_string_pretty(&export)?);
    Ok(())
}

async fn fetch_vyfi_bar_rate(
    dex: VyfiBar,
    pool_identifier: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("[vyfi_bar] fetching rate for pool: {}", pool_identifier);
    let rate = dex.get_rate(pool_identifier).await?;
    println!("{}", serde_json::to_string_pretty(&rate)?);
    Ok(())
}

async fn export_all_vyfinance(dex: VyFinance) -> Result<(), Box<dyn std::error::Error>> {
    let pools = dex.all_liquidity_pools().await?;
    let exports: Vec<PoolExport> = pools.iter().map(|p| pool_to_export(p, "")).collect();

    let mut exports = exports;
    exports.sort_by(|a, b| a.pool_id.cmp(&b.pool_id));

    let json = serde_json::to_string_pretty(&exports)?;
    std::fs::write("pools_rs.json", &json)?;
    eprintln!("Exported {} VyFinance pools to pools_rs.json", exports.len());
    Ok(())
}
