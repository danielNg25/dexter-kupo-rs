use anyhow::Result;
use async_trait::async_trait;
use crate::models::{Utxo, LiquidityPool};
use crate::models::asset::{from_identifier, token_identifier};
use crate::kupo::KupoApi;
use super::BaseDex;

const IDENTIFIER: &str = "MINSWAP";
const LP_TOKEN_POLICY_ID: &str = "e4214b7cce62ac6fbba385d164df48e157eae5863521b4b67ca71d86";
const POOL_NFT_POLICY_ID: &str = "0be55d262b29f564998ff81efe21bdc0022621c12f15af08d0f2ddb1";
// The validity asset is queried as <policy>.<name> — Kupo returns UTXOs containing it
const POOL_VALIDITY_ASSET: &str = "13aa2accf2e1561723aa26871e071fdf32c867cff7e7d50ad470d62f.4d494e53574150";
// Joined form (no dot) used for filtering amounts
const POOL_VALIDITY_ASSET_JOINED: &str = "13aa2accf2e1561723aa26871e071fdf32c867cff7e7d50ad470d62f4d494e53574150";
const POOL_FEE_PERCENT: f64 = 0.3;

pub struct MinswapV1 {
    kupo: KupoApi,
}

impl MinswapV1 {
    pub fn new(kupo: KupoApi) -> Self {
        Self { kupo }
    }
}

#[async_trait]
impl BaseDex for MinswapV1 {
    fn identifier(&self) -> &str {
        IDENTIFIER
    }

    fn pool_address(&self) -> &str {
        // MinswapV1 has no single pool address — pools are indexed by validity asset
        POOL_VALIDITY_ASSET
    }

    fn lp_token_policy_id(&self) -> &str {
        LP_TOKEN_POLICY_ID
    }

    fn kupo(&self) -> &KupoApi {
        &self.kupo
    }

    /// Fetch all pool UTXOs by querying Kupo for the validity asset.
    async fn all_liquidity_pool_utxos(&self) -> Result<Vec<Utxo>> {
        self.kupo.get(POOL_VALIDITY_ASSET, true).await
    }

    /// Build a LiquidityPool from a UTXO.
    /// MinswapV1 does not need a datum — reserves come from UTXO amounts.
    async fn liquidity_pool_from_utxo(&self, utxo: &Utxo, pool_id: &str) -> Result<Option<LiquidityPool>> {
        if utxo.data_hash.is_none() {
            return Ok(None);
        }

        // Filter out the validity asset, LP tokens, and NFT tokens
        let relevant: Vec<_> = utxo.amount.iter().filter(|a| {
            let u = &a.unit;
            u != POOL_VALIDITY_ASSET_JOINED
                && !u.starts_with(LP_TOKEN_POLICY_ID)
                && !u.starts_with(POOL_NFT_POLICY_ID)
        }).collect();

        if relevant.len() < 2 {
            return Ok(None);
        }

        // Pool ID = first asset with the NFT policy
        let pool_id = utxo.amount.iter()
            .find(|a| a.unit.starts_with(POOL_NFT_POLICY_ID))
            .map(|a| a.unit.clone())
            .unwrap_or_else(|| pool_id.to_string());

        // ADA/X pool: index 0 = lovelace, index 1 = token
        // X/X pool:   index 0 = first non-lovelace, skip it, index 1 & 2 are the pair
        let asset_a_idx = if relevant.len() == 2 { 0 } else { 1 };
        let asset_b_idx = if relevant.len() == 2 { 1 } else { 2 };

        let asset_a = from_identifier(&relevant[asset_a_idx].unit, 0);
        let asset_b = from_identifier(&relevant[asset_b_idx].unit, 0);
        let reserve_a = relevant[asset_a_idx].quantity.parse::<u64>()?;
        let reserve_b = relevant[asset_b_idx].quantity.parse::<u64>()?;

        Ok(Some(LiquidityPool::new(
            IDENTIFIER,
            asset_a,
            asset_b,
            reserve_a,
            reserve_b,
            &utxo.address,
            POOL_FEE_PERCENT,
            &pool_id,
        )))
    }

    /// Look up a pool by its NFT pool ID.
    /// Kupo can query by asset directly: GET /matches/<policy>.<name>
    async fn liquidity_pool_from_pool_id(&self, pool_id: &str) -> Result<Option<LiquidityPool>> {
        // Normalise: ensure it has the NFT policy prefix with a dot separator
        let full_id = if pool_id.starts_with(POOL_NFT_POLICY_ID) {
            // Already has policy — ensure dot separator
            if pool_id.len() > 56 && pool_id.chars().nth(56) != Some('.') {
                format!("{}.{}", &pool_id[..56], &pool_id[56..])
            } else {
                pool_id.to_string()
            }
        } else {
            format!("{}.{}", POOL_NFT_POLICY_ID, pool_id)
        };

        let utxos = self.kupo.get(&full_id, true).await?;

        match utxos.first() {
            Some(utxo) => self.liquidity_pool_from_utxo(utxo, &full_id).await,
            None => Ok(None),
        }
    }

    /// Find all pools containing both tokens.
    /// Fetches all V1 pool UTXOs via the validity asset query (same as JS),
    /// then filters client-side. No datum fetch needed.
    async fn liquidity_pools_from_token(&self, token_b: &str, token_a: &str) -> Result<Vec<LiquidityPool>> {
        let all_utxos = self.all_liquidity_pool_utxos().await?;
        eprintln!("[minswap_v1] fetched {} pool UTXOs", all_utxos.len());

        let mut pools = Vec::new();
        for utxo in &all_utxos {
            if let Some(pool) = self.liquidity_pool_from_utxo(utxo, "").await? {
                let id_a = token_identifier(&pool.asset_a);
                let id_b = token_identifier(&pool.asset_b);

                let matches = (id_a == token_a && id_b == token_b)
                    || (id_a == token_b && id_b == token_a);

                if matches {
                    pools.push(pool);
                }
            }
        }

        Ok(pools)
    }
}
