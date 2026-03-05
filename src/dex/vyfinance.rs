use super::cbor::{constr_fields, decode_cbor, value_to_u64};
use super::BaseDex;
use crate::kupo::KupoApi;
use crate::models::asset::from_identifier;
use crate::models::{LiquidityPool, Utxo};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, Semaphore};

const IDENTIFIER: &str = "VyFinance";
const VYFI_API_URL: &str = "https://api.vyfi.io/lp?networkId=1&v2=true";
const CONCURRENCY: usize = 5;

pub struct VyFinance {
    kupo: KupoApi,
    cache: RwLock<Option<VyFinanceCache>>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct VyFinancePoolData {
    pub units_pair: String,
    pub pool_validator_utxo_address: String,
    #[serde(rename = "lpPolicyId-assetId")]
    pub lp_policy_id_asset_id: String,
    pub json: String,
    pub pair: String,
    pub is_live: bool,
    pub order_validator_utxo_address: String,
    #[serde(default)]
    pub pool_nft_policy_id: String,
}

/// Cache structure: HashMap<tokenA, HashMap<tokenB, Vec<VyFinancePoolData>>>
pub type VyFinanceCache = HashMap<String, HashMap<String, Vec<VyFinancePoolData>>>;

impl VyFinance {
    pub fn new(kupo: KupoApi) -> Self {
        Self {
            kupo,
            cache: RwLock::new(None),
        }
    }

    /// Look up the units_pair for a pool_id from the cache.
    async fn find_units_pair_for_pool_id(&self, pool_id: &str) -> Option<String> {
        let guard = self.cache.read().await;
        let cache = guard.as_ref()?;
        for inner in cache.values() {
            for pools in inner.values() {
                for p in pools {
                    if p.pool_nft_policy_id == pool_id {
                        return Some(p.units_pair.clone());
                    }
                }
            }
        }
        None
    }

    /// Ensure the cache is populated. Fetches from VyFi API on first call, reuses after.
    async fn ensure_cache(&self) -> Result<()> {
        // Fast path: cache already populated
        if self.cache.read().await.is_some() {
            return Ok(());
        }
        // Slow path: fetch and populate
        let all = self.fetch_all_pool_data().await?;
        let structured = Self::structure_pool_data(all);
        *self.cache.write().await = Some(structured);
        Ok(())
    }

    /// Fetch all pool metadata from VyFi API.
    pub async fn fetch_all_pool_data(&self) -> Result<Vec<VyFinancePoolData>> {
        let resp = reqwest::get(VYFI_API_URL)
            .await
            .map_err(|e| anyhow!("VyFi API fetch failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(anyhow!("VyFi API returned status {}", resp.status()));
        }

        let mut pools: Vec<VyFinancePoolData> = resp
            .json()
            .await
            .map_err(|e| anyhow!("VyFi API JSON parse failed: {}", e))?;

        for p in pools.iter_mut() {
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
                p.pool_nft_policy_id = format!("{}.{}", cs, tn);
            }
        }

        Ok(pools)
    }

    /// Structure pool data into nested HashMap for fast lookup.
    pub fn structure_pool_data(data: Vec<VyFinancePoolData>) -> VyFinanceCache {
        let mut structured: VyFinanceCache = HashMap::new();

        for pool in data {
            let tokens: Vec<&str> = pool.units_pair.split('/').collect();
            if tokens.len() != 2 {
                continue;
            }
            let token_a = tokens[0].to_string();
            let token_b = tokens[1].to_string();

            structured
                .entry(token_a)
                .or_default()
                .entry(token_b)
                .or_default()
                .push(pool);
        }

        structured
    }

    /// Query pools with optional cache.
    pub async fn liquidity_pools_from_token_cached(
        &self,
        token_b: &str,
        token_a: &str,
        cache: Option<&VyFinanceCache>,
    ) -> Result<Vec<LiquidityPool>> {
        let token_a = token_a.replace('.', "");
        let token_b = token_b.replace('.', "");

        let pool_datas = if let Some(structured) = cache {
            let mut matches = Vec::new();
            if let Some(a_map) = structured.get(&token_a) {
                if let Some(pools) = a_map.get(&token_b) {
                    matches.extend(pools.clone());
                }
            }
            if let Some(b_map) = structured.get(&token_b) {
                if let Some(pools) = b_map.get(&token_a) {
                    matches.extend(pools.clone());
                }
            }
            matches
        } else {
            // Fallback: fetch all and filter (slow)
            let all = self.fetch_all_pool_data().await?;
            all.into_iter()
                .filter(|p| {
                    let tokens: Vec<&str> = p.units_pair.split('/').collect();
                    tokens.len() == 2
                        && ((tokens[0] == token_a && tokens[1] == token_b)
                            || (tokens[0] == token_b && tokens[1] == token_a))
                })
                .collect()
        };

        if pool_datas.is_empty() {
            return Ok(vec![]);
        }

        let kupo_url = self.kupo.api_url().to_string();
        let sem = Arc::new(Semaphore::new(CONCURRENCY));
        let mut handles = Vec::with_capacity(pool_datas.len());

        for pool_data in pool_datas {
            let nft_id = pool_data.pool_nft_policy_id.clone();
            if nft_id.is_empty() {
                continue;
            }
            let pair = pool_data.units_pair.clone();
            let sem = Arc::clone(&sem);
            let kupo_url = kupo_url.clone();

            let handle = tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let kupo = KupoApi::new(&kupo_url);

                let utxos = match kupo.get(&nft_id, true).await {
                    Ok(u) => u,
                    Err(_e) => {
                        return None;
                    }
                };

                let utxo = utxos.into_iter().next()?;
                build_pool_from_utxo(&utxo, &nft_id, &kupo, Some(&pair)).await
            });

            handles.push(handle);
        }

        let mut pools = Vec::new();
        for handle in handles {
            if let Ok(Some(pool)) = handle.await {
                pools.push(pool);
            }
        }

        Ok(pools)
    }
}

/// Build a LiquidityPool from a VyFinance UTXO with datum.
///
/// `units_pair` (e.g. "tokenA/tokenB") provides the canonical token ordering from
/// the VyFi API. The datum's bar_fee fields correspond to this ordering, NOT to
/// the UTXO asset ordering from Kupo. When provided, bar_fees are matched to the
/// correct assets.
async fn build_pool_from_utxo(
    utxo: &Utxo,
    pool_nft_id: &str,
    kupo: &KupoApi,
    units_pair: Option<&str>,
) -> Option<LiquidityPool> {
    if utxo.data_hash.is_none() {
        return None;
    }

    let nft_joined = pool_nft_id.replace('.', "");
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

    let datum = match &utxo.inline_datum {
        Some(d) => d.clone(),
        None => {
            let data_hash = utxo.data_hash.as_ref()?;
            kupo.datum(data_hash).await.ok()?
        }
    };
    let d = parse_pool_datum(&datum).ok()?;

    // Datum bar_fee fields correspond to the units_pair ordering from VyFi API,
    // not the UTXO asset ordering. Check if UTXO asset_a matches the first token
    // in units_pair; if not, swap bar_fees.
    let (fee_for_a, fee_for_b) = if let Some(pair) = units_pair {
        let pair_tokens: Vec<&str> = pair.split('/').collect();
        if pair_tokens.len() == 2 {
            let first_token = pair_tokens[0].replace('.', "");
            let utxo_a_unit = &relevant[a_idx].unit;
            if *utxo_a_unit == first_token {
                (d.bar_fee_a, d.bar_fee_b)
            } else {
                (d.bar_fee_b, d.bar_fee_a)
            }
        } else {
            (d.bar_fee_a, d.bar_fee_b)
        }
    } else {
        (d.bar_fee_a, d.bar_fee_b)
    };

    let reserve_a = raw_a.saturating_sub(fee_for_a);
    let reserve_b = raw_b.saturating_sub(fee_for_b);

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

    Ok(VyFiDatum {
        bar_fee_a,
        bar_fee_b,
        total_lp,
    })
}

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

    async fn all_liquidity_pool_utxos(&self) -> Result<Vec<Utxo>> {
        Ok(vec![])
    }

    async fn liquidity_pool_from_utxo(
        &self,
        utxo: &Utxo,
        pool_id: &str,
    ) -> Result<Option<LiquidityPool>> {
        let units_pair = self.find_units_pair_for_pool_id(pool_id).await;
        Ok(build_pool_from_utxo(utxo, pool_id, &self.kupo, units_pair.as_deref()).await)
    }

    async fn liquidity_pool_from_pool_id(&self, pool_id: &str) -> Result<Option<LiquidityPool>> {
        let nft = if pool_id.contains('.') {
            pool_id.to_string()
        } else if pool_id.len() > 56 {
            format!("{}.{}", &pool_id[..56], &pool_id[56..])
        } else {
            pool_id.to_string()
        };

        let units_pair = self.find_units_pair_for_pool_id(pool_id).await;

        let utxos = self.kupo.get(&nft, true).await?;
        match utxos.first() {
            Some(utxo) => {
                Ok(build_pool_from_utxo(utxo, pool_id, &self.kupo, units_pair.as_deref()).await)
            }
            None => Ok(None),
        }
    }

    async fn liquidity_pools_from_token(
        &self,
        token_b: &str,
        token_a: &str,
    ) -> Result<Vec<LiquidityPool>> {
        self.ensure_cache().await?;
        let guard = self.cache.read().await;
        self.liquidity_pools_from_token_cached(token_b, token_a, guard.as_ref())
            .await
    }

    async fn all_liquidity_pools(&self) -> Result<Vec<LiquidityPool>> {
        let pool_data = self.fetch_all_pool_data().await?;
        let cache = Self::structure_pool_data(pool_data);

        let pool_datas: Vec<VyFinancePoolData> = cache
            .into_values()
            .flat_map(|m| m.into_values())
            .flatten()
            .collect();

        let kupo_url = self.kupo.api_url().to_string();
        let sem = Arc::new(Semaphore::new(CONCURRENCY));
        let mut handles = Vec::with_capacity(pool_datas.len());

        for pool_data in pool_datas {
            let nft_id = pool_data.pool_nft_policy_id.clone();
            if nft_id.is_empty() {
                continue;
            }
            let pair = pool_data.units_pair.clone();
            let sem = Arc::clone(&sem);
            let kupo_url = kupo_url.clone();

            let handle = tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let kupo = KupoApi::new(&kupo_url);
                let utxos = match kupo.get(&nft_id, true).await {
                    Ok(u) => u,
                    Err(_e) => {
                        return None;
                    }
                };
                let utxo = match utxos.into_iter().next() {
                    Some(u) => u,
                    None => {
                        return None;
                    }
                };
                match build_pool_from_utxo(&utxo, &nft_id, &kupo, Some(&pair)).await {
                    Some(pool) => Some(pool),
                    None => None,
                }
            });
            handles.push(handle);
        }

        let mut pools = Vec::new();
        for handle in handles {
            if let Ok(Some(pool)) = handle.await {
                pools.push(pool);
            }
        }
        Ok(pools)
    }
}
