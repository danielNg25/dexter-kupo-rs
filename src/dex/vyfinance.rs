use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::Semaphore;
use crate::kupo::KupoApi;
use crate::models::asset::from_identifier;
use crate::models::{token_identifier, LiquidityPool, Utxo};
use super::BaseDex;
use super::cbor::{constr_fields, decode_cbor, value_to_u64};

const IDENTIFIER: &str = "VYFINANCE";
const VYFI_API_URL: &str = "https://api.vyfi.io/lp?networkId=1&v2=true";
const CONCURRENCY: usize = 5;

pub struct VyFinance {
    kupo: KupoApi,
}

impl VyFinance {
    pub fn new(kupo: KupoApi) -> Self {
        Self { kupo }
    }

    /// Fetch pool list from VyFi API and extract pool NFT policy identifiers.
    pub async fn all_pool_nft_ids(&self) -> Result<Vec<String>> {
        let resp = reqwest::get(VYFI_API_URL)
            .await
            .map_err(|e| anyhow!("VyFi API fetch failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(anyhow!("VyFi API returned status {}", resp.status()));
        }

        let pools: Vec<VyfiPoolEntry> = resp
            .json()
            .await
            .map_err(|e| anyhow!("VyFi API JSON parse failed: {}", e))?;

        let mut nft_ids = Vec::new();
        for p in pools {
            // Parse the embedded JSON string to get mainNFT
            let inner: serde_json::Value =
                serde_json::from_str(&p.json).unwrap_or(serde_json::Value::Null);
            let cs = inner
                .get("mainNFT")
                .and_then(|n| n.get("currencySymbol"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let tn = inner
                .get("mainNFT")
                .and_then(|n| n.get("tokenName"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if !cs.is_empty() {
                nft_ids.push(format!("{}.{}", cs, tn));
            }
        }

        Ok(nft_ids)
    }

    /// Fetch all VyFinance liquidity pools directly.
    /// Overrides the generic export_all path because each pool needs its NFT id
    /// alongside the UTXO to identify the pool_id.
    pub async fn all_liquidity_pools(&self) -> Result<Vec<LiquidityPool>> {
        eprintln!("[vyfinance] fetching pool NFT list from VyFi API...");
        let nft_ids = self.all_pool_nft_ids().await?;
        eprintln!("[vyfinance] found {} pools from API", nft_ids.len());

        let kupo_url = self.kupo.api_url().to_string();
        let nft_ids = Arc::new(nft_ids);
        let sem = Arc::new(Semaphore::new(CONCURRENCY));
        let total = nft_ids.len();

        let mut handles = Vec::with_capacity(total);

        for nft_id in nft_ids.as_ref() {
            let nft_id = nft_id.clone();
            let sem = Arc::clone(&sem);
            // KupoApi is not Clone, so we create a new instance per task using the same URL.
            let kupo_url = kupo_url.clone();

            let handle = tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let kupo = KupoApi::new(&kupo_url);

                let utxos = match kupo.get(&nft_id, true).await {
                    Ok(u) => u,
                    Err(e) => {
                        eprintln!("[vyfinance] kupo error for {}: {}", nft_id, e);
                        return None;
                    }
                };

                let utxo = utxos.into_iter().next()?;
                build_pool_from_utxo(&utxo, &nft_id, &kupo).await
            });

            handles.push(handle);
        }

        let mut pools = Vec::new();
        for handle in handles {
            if let Ok(Some(pool)) = handle.await {
                pools.push(pool);
            }
        }

        eprintln!("[vyfinance] built {} pools", pools.len());
        Ok(pools)
    }
}

/// Build a LiquidityPool from a VyFinance UTXO with datum.
async fn build_pool_from_utxo(utxo: &Utxo, pool_nft_id: &str, kupo: &KupoApi) -> Option<LiquidityPool> {
    if utxo.data_hash.is_none() {
        return None;
    }

    // The NFT asset unit without dot separator
    let nft_joined = pool_nft_id.replace('.', "");

    // Filter out the pool NFT asset itself
    let relevant: Vec<_> = utxo
        .amount
        .iter()
        .filter(|a| a.unit != nft_joined)
        .collect();

    if relevant.len() < 2 {
        return None;
    }

    let a_idx = if relevant.len() == 2 { 0 } else { 1 };
    let b_idx = if relevant.len() == 2 { 1 } else { 2 };

    let raw_a = relevant[a_idx].quantity.parse::<u64>().ok()?;
    let raw_b = relevant[b_idx].quantity.parse::<u64>().ok()?;

    let asset_a = from_identifier(&relevant[a_idx].unit, 0);
    let asset_b = from_identifier(&relevant[b_idx].unit, 0);

    // Fetch datum for BarFee deductions
    let data_hash = utxo.data_hash.as_ref()?;
    let datum = kupo.datum(data_hash).await.ok()?;
    let d = parse_pool_datum(&datum).ok()?;

    let reserve_a = raw_a.saturating_sub(d.bar_fee_a);
    let reserve_b = raw_b.saturating_sub(d.bar_fee_b);

    Some(LiquidityPool {
        dex_identifier: IDENTIFIER.to_string(),
        asset_a,
        asset_b,
        reserve_a,
        reserve_b,
        address: utxo.address.clone(),
        pool_id: pool_nft_id.to_string(),
        pool_fee_percent: 0.3,
        total_lp_tokens: d.total_lp,
    })
}

/// Parse VyFinance pool datum.
///
/// Structure (constructor 0):
///   [0]: int — PoolAssetABarFee → subtract from reserve_a
///   [1]: int — PoolAssetBBarFee → subtract from reserve_b
///   [2]: int — TotalLpTokens
struct VyFiDatum {
    bar_fee_a: u64,
    bar_fee_b: u64,
    total_lp: u64,
}

fn parse_pool_datum(cbor_hex: &str) -> Result<VyFiDatum> {
    let value = decode_cbor(cbor_hex)?;
    let fields = constr_fields(&value)?;

    if fields.len() < 3 {
        return Err(anyhow!(
            "VyFinance datum: expected >=3 fields, got {}",
            fields.len()
        ));
    }

    let bar_fee_a = value_to_u64(&fields[0])?;
    let bar_fee_b = value_to_u64(&fields[1])?;
    let total_lp = value_to_u64(&fields[2])?;

    Ok(VyFiDatum { bar_fee_a, bar_fee_b, total_lp })
}

#[derive(Deserialize)]
struct VyfiPoolEntry {
    json: String,
}

// ── BaseDex boilerplate ───────────────────────────────────────────────────────
// VyFinance uses a completely different discovery path (API → per-NFT Kupo query).
// The BaseDex methods below are stubs; use all_liquidity_pools() directly.

#[async_trait]
impl BaseDex for VyFinance {
    fn identifier(&self) -> &str {
        IDENTIFIER
    }

    fn pool_address(&self) -> &str {
        VYFI_API_URL
    }

    fn lp_token_policy_id(&self) -> &str {
        ""
    }

    fn kupo(&self) -> &KupoApi {
        &self.kupo
    }

    /// VyFinance has no single address — returns empty (use all_liquidity_pools instead).
    async fn all_liquidity_pool_utxos(&self) -> Result<Vec<Utxo>> {
        Ok(vec![])
    }

    async fn liquidity_pool_from_utxo(
        &self,
        utxo: &Utxo,
        pool_id: &str,
    ) -> Result<Option<LiquidityPool>> {
        Ok(build_pool_from_utxo(utxo, pool_id, &self.kupo).await)
    }

    async fn liquidity_pool_from_pool_id(&self, pool_id: &str) -> Result<Option<LiquidityPool>> {
        let nft = if pool_id.contains('.') {
            pool_id.to_string()
        } else if pool_id.len() > 56 {
            format!("{}.{}", &pool_id[..56], &pool_id[56..])
        } else {
            pool_id.to_string()
        };

        let utxos = self.kupo.get(&nft, true).await?;
        match utxos.first() {
            Some(utxo) => Ok(build_pool_from_utxo(utxo, pool_id, &self.kupo).await),
            None => Ok(None),
        }
    }

    async fn liquidity_pools_from_token(
        &self,
        token_b: &str,
        token_a: &str,
    ) -> Result<Vec<LiquidityPool>> {
        let all = self.all_liquidity_pools().await?;
        Ok(all
            .into_iter()
            .filter(|p| {
                let id_a = token_identifier(&p.asset_a);
                let id_b = token_identifier(&p.asset_b);
                (id_a == token_a && id_b == token_b) || (id_a == token_b && id_b == token_a)
            })
            .collect())
    }
}
