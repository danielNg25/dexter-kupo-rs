use anyhow::{anyhow, Result};
use async_trait::async_trait;
use crate::kupo::KupoApi;
use crate::models::asset::from_identifier;
use crate::models::{token_identifier, LiquidityPool, Utxo};
use super::BaseDex;
use super::cbor::{constr_fields, decode_cbor, is_nonempty_constr, value_to_u64};

const IDENTIFIER: &str = "WINGRIDERV2";
const POOL_VALIDITY_POLICY: &str = "6fdc63a1d71dc2c65502b79baae7fb543185702b12c3c5fb639ed737";
const POOL_VALIDITY_ASSET: &str = "6fdc63a1d71dc2c65502b79baae7fb543185702b12c3c5fb639ed737.4c";
const POOL_VALIDITY_ASSET_JOINED: &str =
    "6fdc63a1d71dc2c65502b79baae7fb543185702b12c3c5fb639ed7374c";
const MIN_POOL_ADA: u64 = 3_000_000;

pub struct WingRidersV2 {
    kupo: KupoApi,
}

impl WingRidersV2 {
    pub fn new(kupo: KupoApi) -> Self {
        Self { kupo }
    }
}

/// Parse WingRidersV2 pool datum.
///
/// Structure (constructor 0), 21 top-level fields:
///   [0]:  bytes — RequestScriptHash (ignored)
///   [1]:  bytes — PoolAssetAPolicyId (ignored)
///   [2]:  bytes — PoolAssetAAssetName (ignored)
///   [3]:  bytes — PoolAssetBPolicyId (ignored)
///   [4]:  bytes — PoolAssetBAssetName (ignored)
///   [5]:  int   — SwapFee
///   [6]:  int   — ProtocolFee
///   [7]:  int   — ProjectFeeInBasis
///   [8]:  int   — ReserveFeeInBasis
///   [9]:  int   — FeeBasis (ignored)
///   [10]: int   — AgentFee (ignored)
///   [11]: int   — LastInteraction (ignored)
///   [12]: int   — PoolAssetATreasury
///   [13]: int   — PoolAssetBTreasury
///   [14..19]: misc (ignored)
///   [20]: constr IF stable pool (has WingRidersV2Special) → skip
///
/// Fee = (SwapFee + ProtocolFee + ProjectFeeInBasis + ReserveFeeInBasis) / 100
struct WRV2Datum {
    swap_fee: u64,
    protocol_fee: u64,
    project_fee: u64,
    reserve_fee: u64,
    treasury_a: u64,
    treasury_b: u64,
    project_treasury_a: u64,
    project_treasury_b: u64,
    is_stable: bool,
}

fn parse_pool_datum(cbor_hex: &str) -> Result<WRV2Datum> {
    let value = decode_cbor(cbor_hex)?;
    let fields = constr_fields(&value)?;

    if fields.len() < 14 {
        return Err(anyhow!(
            "WingRidersV2 datum: expected >=14 fields, got {}",
            fields.len()
        ));
    }

    let swap_fee = value_to_u64(&fields[5])?;
    let protocol_fee = value_to_u64(&fields[6])?;
    let project_fee = value_to_u64(&fields[7])?;
    let reserve_fee = value_to_u64(&fields[8])?;
    let treasury_a = value_to_u64(&fields[12])?;
    let treasury_b = value_to_u64(&fields[13])?;

    // Fields [14] and [15] hold the project fee treasury (accumulated from ProjectFeeInBasis).
    // Pools with ProjectFeeInBasis=0 will have these as 0.
    let project_treasury_a = if fields.len() > 14 {
        value_to_u64(&fields[14]).unwrap_or(0)
    } else {
        0
    };
    let project_treasury_b = if fields.len() > 15 {
        value_to_u64(&fields[15]).unwrap_or(0)
    } else {
        0
    };

    // Stable pool detection: last constr field that is non-empty.
    let is_stable = fields.len() > 20 && is_nonempty_constr(&fields[20]);

    Ok(WRV2Datum {
        swap_fee,
        protocol_fee,
        project_fee,
        reserve_fee,
        treasury_a,
        treasury_b,
        project_treasury_a,
        project_treasury_b,
        is_stable,
    })
}

/// Subtract the 3 ADA min-UTXO deposit from pool ADA reserves.
fn ada_reserve(qty: u64) -> u64 {
    qty.saturating_sub(MIN_POOL_ADA)
}

#[async_trait]
impl BaseDex for WingRidersV2 {
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

        let relevant: Vec<_> = utxo
            .amount
            .iter()
            .filter(|a| !a.unit.starts_with(POOL_VALIDITY_POLICY))
            .collect();

        if relevant.len() < 2 {
            return Ok(None);
        }

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

    /// Fetch datum, detect stable pools (skip), update multi-component fee,
    /// and subtract treasury from reserves.
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

        // Skip stable pools (matches JS: returns undefined when WingRidersV2Special found)
        if d.is_stable {
            return Ok(None);
        }

        pool.reserve_a = pool.reserve_a.saturating_sub(d.treasury_a).saturating_sub(d.project_treasury_a);
        pool.reserve_b = pool.reserve_b.saturating_sub(d.treasury_b).saturating_sub(d.project_treasury_b);
        pool.pool_fee_percent =
            (d.swap_fee + d.protocol_fee + d.project_fee + d.reserve_fee) as f64 / 100.0;

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
                    Err(e) => eprintln!("[wingriders_v2] datum error {}: {}", utxo.tx_hash, e),
                }
            }
        }

        Ok(pools)
    }
}
