//! Live Minswap V2 order example — builds + signs + (optionally) submits a
//! real tx on mainnet.
//!
//! USAGE:
//!   cargo run --example live_swap                # dry-run (build+sign, print, no submit)
//!   cargo run --example live_swap -- --submit    # actually broadcast
//!
//! REQUIRED ENV VARS:
//!   KUPO_URL               local Kupo API URL (http://localhost:1442)
//!   BLOCKFROST_PROJECT_ID  mainnet Blockfrost project ID (mainnet...)
//!   SENDER_BECH32          sender mainnet base address   (addr1...)
//!   SENDER_PRIVATE_KEY     raw 64-char hex OR path to cardano-cli payment.skey JSON
//!   POOL_LP_TOKEN_UNIT     V2 pool LP token unit: <lp_policy_56_hex><lp_name_hex>
//!   SWAP_IN_UNIT           "lovelace" or <policy_hex><name_hex>
//!   SWAP_IN_AMOUNT         raw u64 amount to swap in
//!
//! OPTIONAL ENV VARS:
//!   LIMIT_PREMIUM_PERCENT  minimum-receive = estimate * (1 + X/100); default 10.0
//!                          Set to 0 for market order at current price.
//!                          Set to e.g. 5 to require 5% better-than-current output.
//!
//! NOTE: Pool data (assets, reserves, fee) is fetched live from Kupo.
//!       Kupo must be indexing the Minswap V2 validity asset.
//!       Start Kupo with at least:
//!         --match f5808c2c990d86da54bfc97d89cee6efa20cd8461616359478d96b4c.4d5350
//!       (or a broader pattern like --match "*")
//!       AND indexing SENDER_BECH32 for UTxO queries.
//!
//! Use a FRESH TEST WALLET funded with only the ADA you intend to spend.
//! NEVER use your main Eternl wallet's seed phrase or payment key.
//!
//! # Safety
//! - Default mode is DRY-RUN: the tx is built and signed but NOT broadcast.
//! - Pass --submit to actually broadcast. Real ADA will be spent.
//! - The private key is NEVER logged, printed, or echoed in any error message.

use anyhow::{anyhow, bail, Context, Result};
use dexter_kupo_rs::{
    dex::{minswap_v2::MinswapV2, BaseDex},
    models::{token_identifier, Asset, Token},
    DexSwap, KupoApi, SwapRequest,
};
use pallas_addresses::Address as PallasAddress;
use pallas_crypto::{
    hash::{Hash, Hasher},
    key::ed25519,
};
use pallas_txbuilder::{BuildConway, Input, Output, StagingTransaction};
use std::env;

const BLOCKFROST_BASE: &str = "https://cardano-mainnet.blockfrost.io/api/v0";
const CARDANOSCAN_TX: &str = "https://cardanoscan.io/transaction";

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    let submit = args.iter().any(|a| a == "--submit");

    let cfg = Config::from_env().context("loading env vars")?;
    let client = reqwest::Client::new();

    eprintln!("== Live Swap (Minswap V2) ==");
    eprintln!(
        "mode:      {}",
        if submit {
            "SUBMIT — real ADA will be spent"
        } else {
            "DRY-RUN (pass --submit to broadcast)"
        }
    );
    eprintln!("kupo:      {}", cfg.kupo_url);
    eprintln!("blockfrost: (mainnet)");
    eprintln!("sender:    {}", cfg.sender_bech32);
    eprintln!("pool:      {}", cfg.pool_lp_token_unit);
    eprintln!("swap in:   {} of {}", cfg.swap_in_amount, cfg.swap_in_unit);
    eprintln!("limit premium: +{}%", cfg.limit_premium_percent);
    eprintln!();

    // 1. Fetch pool from Kupo, derive min_receive, build the order via the library.
    let kupo = KupoApi::new(&cfg.kupo_url);
    let dex = MinswapV2::new(kupo);

    let pool = dex.liquidity_pool_from_pool_id(&cfg.pool_lp_token_unit).await
        .context("fetching pool from Kupo (is Kupo indexing the V2 validity asset?)")?;
    let pool = pool.ok_or_else(|| anyhow!(
        "no pool found for LP token unit `{}`. Verify (a) the unit is correct and (b) Kupo is indexing the Minswap V2 validity asset (--match f5808c2c990d86da54bfc97d89cee6efa20cd8461616359478d96b4c.4d5350 or broader).",
        cfg.pool_lp_token_unit
    ))?;

    eprintln!("pool fetched from Kupo:");
    eprintln!("  assets:    {} / {}", token_unit(&pool.asset_a), token_unit(&pool.asset_b));
    eprintln!("  reserves:  {} / {}", pool.reserve_a, pool.reserve_b);
    eprintln!("  fee:       {}%", pool.pool_fee_percent);

    // Validate SWAP_IN_UNIT is one of the pool's assets.
    let swap_in = token_from_unit(&cfg.swap_in_unit)?;
    let in_id = token_identifier(&swap_in);
    let a_id = token_identifier(&pool.asset_a);
    let b_id = token_identifier(&pool.asset_b);
    if in_id != a_id && in_id != b_id {
        bail!(
            "SWAP_IN_UNIT `{}` is not in the pool. Pool contains: `{}` and `{}`.",
            cfg.swap_in_unit, a_id, b_id
        );
    }

    // Derive min_receive from estimated output + premium.
    let estimated = dex.estimated_receive(&pool, &swap_in, cfg.swap_in_amount);
    let multiplier = 1.0 + cfg.limit_premium_percent / 100.0;
    let min_receive = (estimated as f64 * multiplier).floor() as u64;

    eprintln!();
    eprintln!("limit order pricing:");
    eprintln!("  current estimate:     {} raw out", estimated);
    eprintln!("  limit premium:        +{}%", cfg.limit_premium_percent);
    eprintln!("  computed min_receive: {}", min_receive);
    eprintln!("  (order only fills if pool gives at least this much output)");
    eprintln!();

    let pays = SwapRequest::new(&dex)
        .for_pool(pool)
        .with_swap_in_token(swap_in)
        .with_swap_in_amount(cfg.swap_in_amount)
        .with_minimum_receive(min_receive)
        .with_sender_address(&cfg.sender_bech32)?
        .build()?;
    let p = &pays[0];
    let datum_hex = p
        .datum
        .as_deref()
        .ok_or_else(|| anyhow!("library produced no datum for order"))?;
    let datum_bytes = hex::decode(datum_hex).context("decoding datum cbor hex")?;
    // Cardano datum hash = blake2b-256 of the raw CBOR bytes of the datum.
    let datum_hash: Hash<32> = Hasher::<256>::hash(&datum_bytes);
    let order_lovelace = p
        .assets
        .iter()
        .find(|a| a.unit == "lovelace")
        .ok_or_else(|| anyhow!("order output missing lovelace"))?
        .quantity;

    eprintln!("order address:  {}", p.address);
    eprintln!(
        "order lovelace: {} ({:.6} ADA)",
        order_lovelace,
        order_lovelace as f64 / 1_000_000.0
    );
    eprintln!("datum hash:     {}", hex::encode(datum_hash));
    eprintln!(
        "datum ({} bytes): {}",
        datum_bytes.len(),
        datum_hex
    );
    eprintln!();

    // 2. Fetch chain state from Blockfrost (params) and Kupo (tip slot, wallet UTxOs).
    let kupo = dex.kupo();

    let params = blockfrost_get(&client, &cfg.bf_project_id, "/epochs/latest/parameters")
        .await
        .context("fetching protocol params")?;
    let min_fee_a = params["min_fee_a"]
        .as_u64()
        .ok_or_else(|| anyhow!("protocol params: min_fee_a missing or not u64"))?;
    let min_fee_b = params["min_fee_b"]
        .as_u64()
        .ok_or_else(|| anyhow!("protocol params: min_fee_b missing or not u64"))?;
    eprintln!("protocol: min_fee_a={} min_fee_b={}", min_fee_a, min_fee_b);

    let current_slot = kupo
        .tip_slot()
        .await
        .context("fetching tip slot from Kupo")?;
    let ttl = current_slot + 7200; // ~2 hours at 1 slot/s
    eprintln!("slot: current={} ttl={}", current_slot, ttl);

    let utxos = kupo
        .get(&cfg.sender_bech32, true)
        .await
        .context("fetching sender UTxOs from Kupo")?;
    eprintln!("kupo returned {} UTxO(s) at sender", utxos.len());
    if utxos.is_empty() {
        bail!(
            "Kupo returned 0 UTxOs for {}. Check: (a) the wallet is actually funded, and (b) Kupo is indexing this address (started with --match <addr> or a broader pattern).",
            cfg.sender_bech32
        );
    }

    // 3. Coin selection — pure-ADA UTxOs only (test wallet should be ADA-only).
    //    Largest-first greedy until we cover order + fee + min change.
    let mut ada_utxos: Vec<(String, u64, u64)> = vec![]; // (tx_hash, output_index, lovelace)
    for u in &utxos {
        // Only consider pure-ADA UTxOs (single unit: lovelace)
        if u.amount.len() != 1 {
            continue; // skip multi-asset UTxOs for simplicity
        }
        if u.amount[0].unit != "lovelace" {
            continue;
        }
        let qty: u64 = u.amount[0]
            .quantity
            .parse()
            .unwrap_or(0);
        ada_utxos.push((u.tx_hash.clone(), u.output_index as u64, qty));
    }
    ada_utxos.sort_by(|a, b| b.2.cmp(&a.2)); // largest-first

    // Initial fee estimate; we refine it after the first build.
    let mut fee_estimate: u64 = 250_000;
    let min_change: u64 = 1_500_000; // min UTxO for change output
    let target = order_lovelace + fee_estimate + min_change;
    let mut selected: Vec<(String, u64, u64)> = vec![];
    let mut total_selected: u64 = 0;
    for u in &ada_utxos {
        if total_selected >= target {
            break;
        }
        selected.push(u.clone());
        total_selected += u.2;
    }
    if total_selected < target {
        bail!(
            "insufficient ADA: wallet has {} lovelace (pure-ADA UTxOs), need at least {}",
            total_selected,
            target
        );
    }
    eprintln!(
        "selected {} input(s) totalling {} lovelace ({:.6} ADA)",
        selected.len(),
        total_selected,
        total_selected as f64 / 1_000_000.0
    );
    eprintln!();

    // 4. Parse destination addresses for pallas.
    let order_addr = PallasAddress::from_bech32(&p.address)
        .map_err(|e| anyhow!("parsing order script address '{}': {}", p.address, e))?;
    let sender_addr = PallasAddress::from_bech32(&cfg.sender_bech32)
        .map_err(|e| anyhow!("parsing sender address: {}", e))?;

    // 5. First build pass: measure serialized tx size.
    let change_first = total_selected - order_lovelace - fee_estimate;
    let built_first = build_tx(
        &selected,
        order_addr.clone(),
        order_lovelace,
        datum_hash,
        sender_addr.clone(),
        change_first,
        datum_bytes.clone(),
        fee_estimate,
        ttl,
    )?;

    // Refine fee from actual tx byte length + signature overhead (~100 bytes).
    let approx_signed_size = built_first.tx_bytes.0.len() as u64 + 100;
    let refined_fee = min_fee_b + min_fee_a * approx_signed_size;
    // Clamp to sane bounds; Cardano linear fee for a small tx is typically 170k–220k.
    fee_estimate = refined_fee.max(170_000).min(500_000);
    eprintln!(
        "fee estimate: {} lovelace (tx ~{} bytes)",
        fee_estimate, approx_signed_size
    );

    // 6. Second build pass with corrected fee (change absorbs the difference).
    if total_selected < order_lovelace + fee_estimate + min_change {
        bail!(
            "insufficient ADA after fee refinement: need {} lovelace total, have {}",
            order_lovelace + fee_estimate + min_change,
            total_selected
        );
    }
    let change_final = total_selected - order_lovelace - fee_estimate;
    let built = build_tx(
        &selected,
        order_addr,
        order_lovelace,
        datum_hash,
        sender_addr,
        change_final,
        datum_bytes,
        fee_estimate,
        ttl,
    )?;

    // 7. Sign.
    let signing_key = parse_signing_key(&cfg.sender_private_key)
        .context("parsing SENDER_PRIVATE_KEY")?;
    let signed = built
        .sign(&signing_key)
        .map_err(|e| anyhow!("signing transaction: {:?}", e))?;

    let tx_cbor = hex::encode(&signed.tx_bytes.0);
    // tx_hash is already computed by pallas in BuiltTransaction.
    let tx_hash = hex::encode(signed.tx_hash.0);

    eprintln!();
    eprintln!("=== Signed transaction ready ===");
    eprintln!("tx hash: {}", tx_hash);
    eprintln!("tx size: {} bytes", signed.tx_bytes.0.len());
    eprintln!("tx cbor: {}", tx_cbor);
    eprintln!(
        "inspect: {}/{}",
        CARDANOSCAN_TX, tx_hash
    );
    eprintln!();

    if !submit {
        eprintln!("DRY-RUN complete. Not broadcasting.");
        eprintln!("Inspect CBOR at https://cbor.me or paste tx hash into Cardanoscan.");
        eprintln!("Re-run with --submit to broadcast.");
        return Ok(());
    }

    // 8. Submit to Blockfrost.
    eprintln!("=== SUBMITTING ===");
    let resp = client
        .post(format!("{}/tx/submit", BLOCKFROST_BASE))
        .header("project_id", &cfg.bf_project_id)
        .header("Content-Type", "application/cbor")
        .body(signed.tx_bytes.0.clone())
        .send()
        .await
        .context("HTTP POST /tx/submit")?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();

    if !status.is_success() {
        bail!("submit failed [{}]: {}", status, body);
    }

    let submitted_hash = body.trim_matches('"');
    eprintln!("submitted!");
    eprintln!("tx hash: {}", submitted_hash);
    eprintln!("{}/{}", CARDANOSCAN_TX, submitted_hash);
    Ok(())
}

// ---------- transaction builder ----------

/// Build a StagingTransaction from pre-selected inputs + computed amounts.
///
/// This is called twice: once for size measurement, once for the final tx.
fn build_tx(
    selected: &[(String, u64, u64)],
    order_addr: PallasAddress,
    order_lovelace: u64,
    datum_hash: Hash<32>,
    sender_addr: PallasAddress,
    change_lovelace: u64,
    datum_bytes: Vec<u8>,
    fee: u64,
    ttl: u64,
) -> Result<pallas_txbuilder::BuiltTransaction> {
    let mut tx = StagingTransaction::new();

    for (h, idx, _) in selected {
        let raw = hex::decode(h)
            .with_context(|| format!("decoding input tx_hash hex: {}", h))?;
        let arr: [u8; 32] = raw
            .try_into()
            .map_err(|_| anyhow!("tx_hash is not 32 bytes: {}", h))?;
        tx = tx.input(Input::new(Hash::<32>::from(arr), *idx));
    }

    tx = tx
        // Order output (script address, datum hash attached).
        .output(
            Output::new(order_addr, order_lovelace)
                .set_datum_hash(datum_hash),
        )
        // Change back to sender.
        .output(Output::new(sender_addr, change_lovelace))
        // Add datum bytes to the tx witness set so it's visible on-chain.
        // (pallas stores it keyed by hash_cbor; batcher picks it up via
        //  its own indexer, so a key discrepancy doesn't affect correctness.)
        .datum(datum_bytes)
        .fee(fee)
        .invalid_from_slot(ttl)
        .network_id(1);

    tx.build_conway_raw()
        .map_err(|e| anyhow!("build_conway_raw: {:?}", e))
}

// ---------- helpers ----------

fn token_from_unit(unit: &str) -> Result<Token> {
    if unit == "lovelace" {
        return Ok(Token::Lovelace);
    }
    if unit.len() < 56 {
        bail!(
            "token unit '{}' is too short (need at least 56 hex chars for policy ID)",
            unit
        );
    }
    let (policy, name) = unit.split_at(56);
    Ok(Token::Asset(Asset::new(policy, name, 0)))
}

fn token_unit(t: &Token) -> String {
    match t {
        Token::Lovelace => "lovelace".to_string(),
        Token::Asset(a) => format!("{}{}", a.policy_id, a.name_hex),
    }
}

/// Parse an Ed25519 secret key from either:
///   - a raw 64-character hex string (32 bytes), OR
///   - a path to a cardano-cli `payment.skey` JSON file
///     (cborHex = 0x5820 || 32-byte key).
///
/// SECURITY: This function NEVER echoes key bytes in error messages.
fn parse_signing_key(value: &str) -> Result<ed25519::SecretKey> {
    let trimmed = value.trim();

    // Branch 1: 64-char hex (32-byte raw signing key).
    if trimmed.len() == 64 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        let bytes = hex::decode(trimmed).map_err(|_| anyhow!("invalid private key format"))?;
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow!("invalid private key format"))?;
        return Ok(ed25519::SecretKey::from(arr));
    }

    // Branch 2: file path to a cardano-cli payment.skey JSON.
    let path = std::path::Path::new(trimmed);
    if path.exists() {
        let json_text =
            std::fs::read_to_string(path).map_err(|_| anyhow!("invalid private key format"))?;
        let parsed: serde_json::Value =
            serde_json::from_str(&json_text).map_err(|_| anyhow!("invalid private key format"))?;
        let cbor_hex = parsed["cborHex"]
            .as_str()
            .ok_or_else(|| anyhow!("invalid private key format"))?;
        // cardano-cli encodes the 32-byte key as: 0x5820 <32 bytes>
        let cbor_bytes =
            hex::decode(cbor_hex).map_err(|_| anyhow!("invalid private key format"))?;
        if cbor_bytes.len() != 34 || cbor_bytes[0] != 0x58 || cbor_bytes[1] != 0x20 {
            bail!("invalid private key format");
        }
        let arr: [u8; 32] = cbor_bytes[2..]
            .try_into()
            .map_err(|_| anyhow!("invalid private key format"))?;
        return Ok(ed25519::SecretKey::from(arr));
    }

    bail!("invalid private key format")
}

// ---------- config ----------

struct Config {
    kupo_url: String,
    bf_project_id: String,
    sender_bech32: String,
    /// Raw env-var value; parsed lazily by `parse_signing_key`.
    sender_private_key: String,
    pool_lp_token_unit: String,
    swap_in_unit: String,
    swap_in_amount: u64,
    /// Percentage premium over the current estimated output required to fill.
    /// Defaults to 10.0 if `LIMIT_PREMIUM_PERCENT` is unset.
    limit_premium_percent: f64,
}

impl Config {
    fn from_env() -> Result<Self> {
        const REQUIRED: &[&str] = &[
            "KUPO_URL",
            "BLOCKFROST_PROJECT_ID",
            "SENDER_BECH32",
            "SENDER_PRIVATE_KEY",
            "POOL_LP_TOKEN_UNIT",
            "SWAP_IN_UNIT",
            "SWAP_IN_AMOUNT",
        ];

        let missing: Vec<&str> = REQUIRED
            .iter()
            .copied()
            .filter(|k| env::var(k).is_err())
            .collect();

        if !missing.is_empty() {
            bail!("missing required env vars: {:?}", missing);
        }

        let kupo_url = env::var("KUPO_URL").unwrap();
        let bf_project_id = env::var("BLOCKFROST_PROJECT_ID").unwrap();
        let sender_bech32 = env::var("SENDER_BECH32").unwrap();

        // Early-fail on wrong-network env values.
        if !sender_bech32.starts_with("addr1") {
            bail!(
                "SENDER_BECH32 must be a mainnet base address starting with `addr1` — got `{}` (testnet/preview addresses are not supported)",
                sender_bech32
            );
        }

        if !bf_project_id.starts_with("mainnet") {
            bail!(
                "BLOCKFROST_PROJECT_ID must be a mainnet project id (starting with `mainnet`) — got a `{}...` prefix",
                bf_project_id.chars().take(7).collect::<String>()
            );
        }

        let limit_premium_percent = env::var("LIMIT_PREMIUM_PERCENT")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(10.0);

        Ok(Self {
            kupo_url,
            bf_project_id,
            sender_bech32,
            sender_private_key: env::var("SENDER_PRIVATE_KEY").unwrap(),
            pool_lp_token_unit: env::var("POOL_LP_TOKEN_UNIT").unwrap(),
            swap_in_unit: env::var("SWAP_IN_UNIT").unwrap(),
            swap_in_amount: env::var("SWAP_IN_AMOUNT")
                .unwrap()
                .parse::<u64>()
                .context("SWAP_IN_AMOUNT must be a u64")?,
            limit_premium_percent,
        })
    }
}

// ---------- Blockfrost ----------

async fn blockfrost_get(
    client: &reqwest::Client,
    project_id: &str,
    path: &str,
) -> Result<serde_json::Value> {
    let url = format!("{}{}", BLOCKFROST_BASE, path);
    let resp = client
        .get(&url)
        .header("project_id", project_id)
        .send()
        .await
        .with_context(|| format!("GET {}", url))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .unwrap_or_else(|_| "<unreadable body>".into());

    if !status.is_success() {
        bail!("Blockfrost GET {} returned {}: {}", path, status, body);
    }

    serde_json::from_str(&body)
        .with_context(|| format!("parsing JSON from Blockfrost GET {}", path))
}
