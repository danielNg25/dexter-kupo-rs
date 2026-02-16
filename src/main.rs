mod dex;
mod kupo;
mod models;
mod utils;

use crate::models::asset::token_identifier;
use dex::cswap::CSwap;
use dex::minswap_v1::MinswapV1;
use dex::minswap_v2::MinswapV2;
use dex::sundaeswap_v1::SundaeSwapV1;
use dex::sundaeswap_v3::SundaeSwapV3;
use dex::vyfinance::VyFinance;
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
        "  {} [--dex <dex_name>] [asset_a asset_b]",
        bin
    );
    eprintln!();
    eprintln!("  No args          → export all pools to pools_rs.json");
    eprintln!("  asset_a asset_b  → print matching pools to stdout");
    eprintln!("  --dex            → choose DEX (default: minswap_v2)");
    eprintln!();
    eprintln!("  Available DEXes:");
    eprintln!("    minswap_v1, minswap_v2");
    eprintln!("    sundaeswap_v1, sundaeswap_v3");
    eprintln!("    wingriders, wingriders_v2");
    eprintln!("    cswap");
    eprintln!("    vyfinance");
    eprintln!();
    eprintln!("  Use 'lovelace' for ADA.");
    eprintln!("  Example:");
    eprintln!("    cargo run --release -- --dex minswap_v1 lovelace f13ac4d66b3ee19a6aa0f2a22298737bd907cc95121662fc971b5275535452494b45");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let raw_args: Vec<String> = std::env::args().collect();

    // Parse --dex flag and positional asset args
    let mut dex_name = "minswap_v2".to_string();
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
        } else {
            assets.push(raw_args[i].clone());
        }
        i += 1;
    }

    let kupo = KupoApi::new(KUPO_URL);

    match dex_name.as_str() {
        "minswap_v1" => run(MinswapV1::new(kupo), &assets, &raw_args[0]).await?,
        "minswap_v2" => run(MinswapV2::new(kupo), &assets, &raw_args[0]).await?,
        "sundaeswap_v1" => run(SundaeSwapV1::new(kupo), &assets, &raw_args[0]).await?,
        "sundaeswap_v3" => run(SundaeSwapV3::new(kupo), &assets, &raw_args[0]).await?,
        "wingriders" => run(WingRiders::new(kupo), &assets, &raw_args[0]).await?,
        "wingriders_v2" => run(WingRidersV2::new(kupo), &assets, &raw_args[0]).await?,
        "cswap" => run(CSwap::new(kupo), &assets, &raw_args[0]).await?,
        "vyfinance" => {
            // VyFinance uses a special path: API discovery → per-pool Kupo queries
            let dex = VyFinance::new(kupo);
            if assets.len() == 2 {
                fetch_pair(dex, &assets[0], &assets[1]).await?;
            } else if assets.is_empty() {
                export_all_vyfinance(dex).await?;
            } else {
                print_usage(&raw_args[0]);
                std::process::exit(1);
            }
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
                // extend fetches the datum for accurate reserves/fee
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

/// VyFinance-specific export: uses the API-discovery path, bypasses generic export_all.
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
