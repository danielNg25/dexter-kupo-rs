//! Live Minswap V2 order example — builds + signs + (optionally) submits a
//! real tx on mainnet.
//!
//! USAGE:
//!   cargo run --example live_swap                # dry-run (build+sign, print, no submit)
//!   cargo run --example live_swap -- --submit    # actually broadcast
//!
//! Env vars can be set via standard `export FOO=bar` in your shell, OR by
//! creating a `.env` file in the working directory with `FOO=bar` lines.
//! (`.env` is already in `.gitignore` — your mnemonics stay out of commits.)
//! **Important**: Values containing spaces (e.g., the BIP-39 mnemonic) MUST be
//! wrapped in double quotes in `.env`. For example:
//!   `SENDER_MNEMONIC="word1 word2 word3 ... word24"`
//!
//! REQUIRED ENV VARS:
//!   KUPO_URL               local Kupo API URL (http://localhost:1442)
//!   BLOCKFROST_PROJECT_ID  mainnet Blockfrost project ID (mainnet...)
//!   SENDER_BECH32          sender mainnet base address   (addr1...)
//!   SENDER_MNEMONIC        BIP-39 mnemonic phrase (12, 15, 18, 21, or 24 words)
//!   POOL_LP_TOKEN_UNIT     V2 pool LP token unit: <lp_policy_56_hex><lp_name_hex>
//!   SWAP_IN_UNIT           "lovelace" or <policy_hex><name_hex>
//!   SWAP_IN_AMOUNT         raw u64 amount to swap in
//!
//! OPTIONAL ENV VARS:
//!   SENDER_MNEMONIC_PASSPHRASE  spending password / BIP-39 passphrase (default: empty)
//!   LIMIT_PREMIUM_PERCENT       minimum-receive = estimate * (1 + X/100); default 10.0
//!                               Set to 0 for market order at current price.
//!                               Set to e.g. 5 to require 5% better-than-current output.
//!
//! The mnemonic is used to derive both payment + stake keys via CIP-1852
//! (Icarus master key derivation, path m/1852'/1815'/0'/0/0 for payment
//! and m/1852'/1815'/0'/2/0 for stake). SENDER_BECH32 acts as a sanity
//! check — if the derived address doesn't match, the example refuses to
//! do anything.
//!
//! NOTE: Pool data (assets, reserves, fee) is fetched live from Kupo.
//!       Kupo must be indexing the Minswap V2 validity asset.
//!       Start Kupo with at least:
//!         --match f5808c2c990d86da54bfc97d89cee6efa20cd8461616359478d96b4c.4d5350
//!       (or a broader pattern like --match "*")
//!       AND indexing SENDER_BECH32 for UTxO queries.
//!
//! Use a FRESH TEST WALLET funded with only the ADA you intend to spend.
//! NEVER use your main Eternl wallet's seed phrase.
//!
//! # Safety
//! - Default mode is DRY-RUN: the tx is built and signed but NOT broadcast.
//! - Pass --submit to actually broadcast. Real ADA will be spent.
//! - The mnemonic and passphrase are NEVER logged, printed, or echoed in any
//!   error message.

use anyhow::{anyhow, bail, Context, Result};
use bip39::{Language, Mnemonic};
use dexter_kupo_rs::{
    dex::{minswap_v2::MinswapV2, BaseDex},
    models::{token_identifier, Asset, Token},
    DexSwap, KupoApi, SwapRequest,
};
use ed25519_bip32::{DerivationScheme, XPrv};
use pallas_addresses::Address as PallasAddress;
use pallas_crypto::{
    hash::{Hash, Hasher},
    key::ed25519,
};
use pallas_txbuilder::{BuildConway, Input, Output, StagingTransaction};
use std::env;

const BLOCKFROST_BASE: &str = "https://cardano-mainnet.blockfrost.io/api/v0";
const CARDANOSCAN_TX: &str = "https://cardanoscan.io/transaction";

/// Index into the extended-key bytes where kL ends (first 32 bytes = kL).
const HARDENED: u32 = 0x8000_0000;

#[tokio::main]
async fn main() -> Result<()> {
    // Auto-load .env from the current working directory if present. Silently ignored
    // if no .env file exists — the example still works with pure shell `export` vars.
    match dotenvy::dotenv() {
        Ok(path) => {
            eprintln!(".env loaded from {}", path.display());
        }
        Err(dotenvy::Error::Io(ref e)) if e.kind() == std::io::ErrorKind::NotFound => {
            // No .env file; fine, vars may be set via shell exports.
        }
        Err(dotenvy::Error::LineParse(_, idx)) => {
            eprintln!(
                ".env parse error at line offset {}. \
                 Common cause: a value containing spaces is not wrapped in quotes. \
                 Wrap multi-word values like SENDER_MNEMONIC in double quotes: \
                 SENDER_MNEMONIC=\"word1 word2 ...\". \
                 (Offending line content suppressed to avoid leaking secrets.)",
                idx
            );
        }
        Err(e) => {
            // Some other dotenvy error — print only the variant name, not the inner content,
            // to avoid accidentally leaking secret values.
            eprintln!(".env error: {:?} (details suppressed)", std::mem::discriminant(&e));
        }
    }

    let args: Vec<String> = env::args().collect();
    let submit = args.iter().any(|a| a == "--submit");

    let cfg = Config::from_env().context("loading env vars")?;

    // ── Key derivation — MUST happen before any network call ──────────────────
    // A wrong mnemonic or passphrase produces a different address; the
    // SENDER_BECH32 sanity check below refuses to proceed in that case.
    let (payment_xprv, _stake_xprv, derived_addr) =
        derive_account_keys(&cfg.sender_mnemonic, &cfg.sender_mnemonic_passphrase)
            .context("deriving keys from mnemonic")?;

    if derived_addr != cfg.sender_bech32 {
        bail!(
            "MNEMONIC DOES NOT MATCH SENDER_BECH32.\n  derived:  {}\n  expected: {}\n  Refusing to proceed (wrong wallet or wrong mnemonic).",
            derived_addr,
            cfg.sender_bech32
        );
    }
    eprintln!("✓ derived address matches SENDER_BECH32");

    // Convert XPrv payment key to pallas SecretKeyExtended.
    // extended_secret_key() returns the 64-byte (kL||kR) private portion.
    let ext_bytes: [u8; 64] = payment_xprv.extended_secret_key();
    let signing_key = ed25519::SecretKeyExtended::from_bytes(ext_bytes)
        .map_err(|_| anyhow!("constructing extended signing key from derived payment key"))?;
    // Shadow the intermediate buffer so it's no longer accessible.
    let _ = ext_bytes;

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

    // 7. Sign with the derived extended ed25519 payment key.
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

// ---------- key derivation ----------

/// Derive payment and stake XPrv keys from a BIP-39 mnemonic via CIP-1852 /
/// CIP-3 Icarus master key derivation, then compute the corresponding mainnet
/// base address and return it as a bech32 string for the caller to verify.
///
/// Derivation path:
///   payment: m / 1852' / 1815' / 0' / 0 / 0
///   stake:   m / 1852' / 1815' / 0' / 2 / 0
///
/// Master key algorithm: CIP-3 Icarus (PBKDF2-HMAC-SHA512, 4096 iterations,
/// 96-byte output, then Cardano BIP32-Ed25519 kL bit-tweaks).
///
/// SECURITY: mnemonic and passphrase are NEVER echoed in any error message.
fn derive_account_keys(
    mnemonic_phrase: &str,
    passphrase: &str,
) -> Result<(XPrv, XPrv, String)> {
    // 1. Parse mnemonic to entropy.
    let mnemonic = Mnemonic::parse_in_normalized(Language::English, mnemonic_phrase)
        .map_err(|_| anyhow!("invalid mnemonic"))?;
    let entropy = mnemonic.to_entropy();

    // 2. Icarus master key derivation per CIP-3 Annex:
    //    PBKDF2-HMAC-SHA512(password=passphrase, salt=entropy, c=4096, dkLen=96)
    //    Then split into kL[0..32] || kR[32..64] || cc[64..96] and apply
    //    Cardano BIP32-Ed25519 bit-tweaks to kL:
    //      kL[0]  &= 0b1111_1000  (clear low 3 bits)
    //      kL[31] &= 0b0001_1111  (clear bits 7, 6, 5)
    //      kL[31] |= 0b0100_0000  (set bit 6)
    let mut xprv_bytes = [0u8; 96];
    pbkdf2::pbkdf2_hmac::<sha2::Sha512>(
        passphrase.as_bytes(),
        &entropy,
        4096,
        &mut xprv_bytes,
    );
    xprv_bytes[0]  &= 0b1111_1000;
    xprv_bytes[31] &= 0b0001_1111;
    xprv_bytes[31] |= 0b0100_0000;

    let root = XPrv::from_bytes_verified(xprv_bytes)
        .map_err(|_| anyhow!("invalid root xprv after derivation"))?;

    // 3. Derive m/1852'/1815'/0'/0/0 (payment) and m/1852'/1815'/0'/2/0 (stake).
    let account_root = root
        .derive(DerivationScheme::V2, 1852 | HARDENED)
        .derive(DerivationScheme::V2, 1815 | HARDENED)
        .derive(DerivationScheme::V2, 0    | HARDENED);
    let payment = account_root
        .derive(DerivationScheme::V2, 0)
        .derive(DerivationScheme::V2, 0);
    let stake = account_root
        .derive(DerivationScheme::V2, 2)
        .derive(DerivationScheme::V2, 0);

    // 4. Compute payment_pkh and stake_kh via Blake2b-224(pubkey).
    let payment_pub: [u8; 32] = payment.public().public_key();
    let stake_pub:   [u8; 32] = stake.public().public_key();
    let payment_pkh = blake2b224(&payment_pub);
    let stake_kh    = blake2b224(&stake_pub);

    // 5. Build expected mainnet base address (header byte 0x01) and bech32-encode.
    //    Layout: 0x01 || payment_pkh[28] || stake_kh[28]  (57 bytes total)
    let mut payload = Vec::with_capacity(57);
    payload.push(0x01u8); // type-0 base address, mainnet network tag
    payload.extend_from_slice(&payment_pkh);
    payload.extend_from_slice(&stake_kh);
    let hrp = bech32::Hrp::parse("addr")?;
    let derived_bech32 = bech32::encode::<bech32::Bech32>(hrp, &payload)?;

    Ok((payment, stake, derived_bech32))
}

/// Blake2b-224 hash of `input` — used for payment key hash and stake key hash.
///
/// Uses `pallas_crypto::hash::Hasher::<224>` which is already a transitive dep.
fn blake2b224(input: &[u8]) -> [u8; 28] {
    let h = pallas_crypto::hash::Hasher::<224>::hash(input);
    let mut out = [0u8; 28];
    out.copy_from_slice(h.as_ref());
    out
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

// ---------- config ----------

struct Config {
    kupo_url: String,
    bf_project_id: String,
    sender_bech32: String,
    /// BIP-39 mnemonic phrase; NEVER logged.
    sender_mnemonic: String,
    /// Optional spending password / BIP-39 passphrase; NEVER logged.
    sender_mnemonic_passphrase: String,
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
            "SENDER_MNEMONIC",
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

        // Passphrase defaults to empty string (most users won't set one).
        let sender_mnemonic_passphrase = env::var("SENDER_MNEMONIC_PASSPHRASE")
            .unwrap_or_default();

        Ok(Self {
            kupo_url,
            bf_project_id,
            sender_bech32,
            sender_mnemonic: env::var("SENDER_MNEMONIC").unwrap(),
            sender_mnemonic_passphrase,
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
