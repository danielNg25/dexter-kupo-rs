use anyhow::{anyhow, Result};
use async_trait::async_trait;
use crate::kupo::KupoApi;
use crate::models::asset::from_identifier;
use crate::models::{token_identifier, LiquidityPool, Utxo};
use super::BaseDex;
use super::cbor::{constr_fields, decode_cbor, value_to_u64};

const IDENTIFIER: &str = "WINGRIDER";
const POOL_VALIDITY_POLICY: &str = "026a18d04a0c642759bb3d83b12e3344894e5c1c7b2aeb1a2113a570";
/// Kupo query pattern (policy.name with dot)
const POOL_VALIDITY_ASSET: &str = "026a18d04a0c642759bb3d83b12e3344894e5c1c7b2aeb1a2113a570.4c";
/// Joined form (no dot) — the "check" asset to exclude when finding pool_id
const POOL_VALIDITY_ASSET_JOINED: &str =
    "026a18d04a0c642759bb3d83b12e3344894e5c1c7b2aeb1a2113a5704c";
/// Minimum ADA locked in pool (3 ADA)
const MIN_POOL_ADA: u64 = 3_000_000;

pub struct WingRiders {
    kupo: KupoApi,
}

impl WingRiders {
    pub fn new(kupo: KupoApi) -> Self {
        Self { kupo }
    }
}

/// Parse WingRiders pool datum.
///
/// Structure (constructor 0):
///   [0]: bytes — RequestScriptHash (ignored)
///   [1]: constr (constructor 0):
///          [0]: constr — asset pair (ignored)
///          [1]: int    — LastInteraction (ignored)
///          [2]: int    — PoolAssetATreasury
///          [3]: int    — PoolAssetBTreasury
struct WRDatum {
    treasury_a: u64,
    treasury_b: u64,
}

fn parse_pool_datum(cbor_hex: &str) -> Result<WRDatum> {
    let value = decode_cbor(cbor_hex)?;
    let top = constr_fields(&value)?;

    if top.len() < 2 {
        return Err(anyhow!("WingRiders datum: expected >=2 top fields, got {}", top.len()));
    }

    // top[1] is the inner constr
    let inner = constr_fields(&top[1])?;
    if inner.len() < 4 {
        return Err(anyhow!("WingRiders inner datum: expected >=4 fields, got {}", inner.len()));
    }

    let treasury_a = value_to_u64(&inner[2])?;
    let treasury_b = value_to_u64(&inner[3])?;

    Ok(WRDatum { treasury_a, treasury_b })
}

/// Subtract the 3 ADA min-UTXO deposit from pool ADA reserves.
fn ada_reserve(qty: u64) -> u64 {
    qty.saturating_sub(MIN_POOL_ADA)
}

#[async_trait]
impl BaseDex for WingRiders {
    fn identifier(&self) -> &str {
        IDENTIFIER
    }

    fn pool_address(&self) -> &str {
        POOL_VALIDITY_ASSET
    }

    fn lp_token_policy_id(&self) -> &str {
        POOL_VALIDITY_POLICY
    }

    fn kupo(&self) -> &KupoApi {
        &self.kupo
    }

    async fn all_liquidity_pool_utxos(&self) -> Result<Vec<Utxo>> {
        self.kupo.get(POOL_VALIDITY_ASSET, true).await
    }

    async fn liquidity_pool_from_utxo(
        &self,
        utxo: &Utxo,
        pool_id: &str,
    ) -> Result<Option<LiquidityPool>> {
        if utxo.data_hash.is_none() {
            return Ok(None);
        }

        // Filter out all validity policy assets
        let relevant: Vec<_> = utxo
            .amount
            .iter()
            .filter(|a| !a.unit.starts_with(POOL_VALIDITY_POLICY))
            .collect();

        if relevant.len() < 2 {
            return Ok(None);
        }

        // Pool ID = first validity policy asset that is NOT the validity check asset
        let pool_id = utxo
            .amount
            .iter()
            .find(|a| {
                a.unit.starts_with(POOL_VALIDITY_POLICY)
                    && a.unit != POOL_VALIDITY_ASSET_JOINED
            })
            .map(|a| a.unit.clone())
            .unwrap_or_else(|| pool_id.to_string());

        let a_idx = if relevant.len() == 2 { 0 } else { 1 };
        let b_idx = if relevant.len() == 2 { 1 } else { 2 };

        let a_unit = &relevant[a_idx].unit;
        let b_unit = &relevant[b_idx].unit;

        let raw_a = relevant[a_idx].quantity.parse::<u64>()?;
        let raw_b = relevant[b_idx].quantity.parse::<u64>()?;

        let reserve_a = if a_unit == "lovelace" { ada_reserve(raw_a) } else { raw_a };
        let reserve_b = if b_unit == "lovelace" { ada_reserve(raw_b) } else { raw_b };

        let asset_a = from_identifier(a_unit, 0);
        let asset_b = from_identifier(b_unit, 0);

        Ok(Some(LiquidityPool::new(
            IDENTIFIER,
            asset_a,
            asset_b,
            reserve_a,
            reserve_b,
            &utxo.address,
            0.35,
            &pool_id,
        )))
    }

    /// Fetch datum and subtract treasury from reserves.
    async fn liquidity_pool_from_utxo_extend(
        &self,
        utxo: &Utxo,
        pool_id: &str,
    ) -> Result<Option<LiquidityPool>> {
        let mut pool = match self.liquidity_pool_from_utxo(utxo, pool_id).await? {
            Some(p) => p,
            None => return Ok(None),
        };

        let data_hash = match &utxo.data_hash {
            Some(h) => h,
            None => return Ok(None),
        };

        let datum = self.kupo.datum(data_hash).await?;
        let d = parse_pool_datum(&datum)?;

        pool.reserve_a = pool.reserve_a.saturating_sub(d.treasury_a);
        pool.reserve_b = pool.reserve_b.saturating_sub(d.treasury_b);

        Ok(Some(pool))
    }

    async fn liquidity_pool_from_pool_id(&self, pool_id: &str) -> Result<Option<LiquidityPool>> {
        let full_id = if pool_id.starts_with(POOL_VALIDITY_POLICY) {
            pool_id.to_string()
        } else {
            format!("{}{}", POOL_VALIDITY_POLICY, pool_id)
        };

        let utxos = self.all_liquidity_pool_utxos().await?;
        let found = utxos
            .iter()
            .find(|u| u.amount.iter().any(|a| a.unit == full_id));

        match found {
            Some(utxo) => self.liquidity_pool_from_utxo_extend(utxo, &full_id).await,
            None => Ok(None),
        }
    }

    async fn liquidity_pools_from_token(
        &self,
        token_b: &str,
        token_a: &str,
    ) -> Result<Vec<LiquidityPool>> {
        let all_utxos = self.all_liquidity_pool_utxos().await?;
        let mut pools = Vec::new();

        for utxo in &all_utxos {
            let base = match self.liquidity_pool_from_utxo(utxo, "").await? {
                Some(p) => p,
                None => continue,
            };

            let id_a = token_identifier(&base.asset_a);
            let id_b = token_identifier(&base.asset_b);

            let matches = (id_a == token_a && id_b == token_b)
                || (id_a == token_b && id_b == token_a);

            if matches {
                match self
                    .liquidity_pool_from_utxo_extend(utxo, &base.pool_id)
                    .await
                {
                    Ok(Some(p)) => pools.push(p),
                    Ok(None) => {}
                    Err(e) => eprintln!("[wingriders] datum error {}: {}", utxo.tx_hash, e),
                }
            }
        }

        Ok(pools)
    }
}
