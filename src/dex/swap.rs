//! `DexSwap` — the write-side trait (order construction).
//!
//! Kept separate from `BaseDex` (reading) so the reading and writing surfaces
//! don't entangle. A DEX impl can implement both.

use anyhow::{anyhow, Result};

use crate::models::{LiquidityPool, Token, Utxo};
use crate::requests::{PayToAddress, SwapFee, SwapParams};

pub trait DexSwap: Send + Sync {
    fn identifier(&self) -> &str;

    fn estimated_receive(&self, pool: &LiquidityPool, in_token: &Token, in_amount: u64) -> u64;
    fn estimated_give(&self, pool: &LiquidityPool, out_token: &Token, out_amount: u64) -> u64;
    fn price_impact_percent(&self, pool: &LiquidityPool, in_token: &Token, in_amount: u64) -> f64;
    fn swap_order_fees(&self) -> Vec<SwapFee>;

    fn build_swap_order(
        &self,
        pool: &LiquidityPool,
        params: &SwapParams,
    ) -> Result<Vec<PayToAddress>>;

    fn build_cancel_order(
        &self,
        order_utxos: &[Utxo],
        return_address: &str,
    ) -> Result<Vec<PayToAddress>>;

    /// Bundle multiple order actions into a single `BulkOrderPlan`.
    ///
    /// Default impl composes `build_swap_order` + `build_cancel_order` per
    /// item and aggregates all inputs into `cancels` and all new outputs into
    /// `new_orders`. Updates produce one cancel-half + one place-half.
    fn build_bulk_orders(
        &self,
        pool: Option<&crate::models::LiquidityPool>,
        cancels: &[crate::models::Utxo],
        swaps: &[crate::requests::types::SwapParams],
        updates: &[(crate::models::Utxo, crate::requests::types::SwapParams)],
        return_address: &str,
    ) -> anyhow::Result<crate::requests::bulk::BulkOrderPlan> {
        use crate::requests::bulk::BulkOrderPlan;

        let mut all_cancels: Vec<crate::requests::types::SpendUtxo> = Vec::new();
        let mut all_new_orders: Vec<crate::requests::types::PayToAddress> = Vec::new();

        // Resolve pool once for the swap/update paths. Pure-cancel bundles
        // never reach the loops below, so missing-pool there is fine.
        let pool_for_swaps = || -> anyhow::Result<&crate::models::LiquidityPool> {
            pool.ok_or_else(|| anyhow!(
                "build_bulk_orders: pool is required when the bundle has at least one swap or update"
            ))
        };

        // Pure cancels
        for utxo in cancels {
            let cancel_pays = self.build_cancel_order(&[utxo.clone()], return_address)?;
            for pay in cancel_pays {
                if pay.spend_utxos.is_empty() {
                    return Err(anyhow!(
                        "build_bulk_orders: build_cancel_order returned a PayToAddress with empty spend_utxos for cancel of {}; expected at least one script input",
                        utxo.tx_hash
                    ));
                }
                for spend in pay.spend_utxos {
                    all_cancels.push(spend);
                }
            }
        }

        // Pure swaps
        for swap in swaps {
            let pays = self.build_swap_order(pool_for_swaps()?, swap)?;
            if pays.is_empty() {
                anyhow::bail!("build_swap_order returned empty list for a bulk swap item");
            }
            for p in &pays {
                if !p.spend_utxos.is_empty() {
                    return Err(anyhow!(
                        "build_bulk_orders: build_swap_order returned a PayToAddress with non-empty spend_utxos ({} entries); the bulk path assumes new orders carry no script inputs of their own — DEX impls that need co-inputs must override build_bulk_orders",
                        p.spend_utxos.len()
                    ));
                }
            }
            all_new_orders.extend(pays);
        }

        // Updates: cancel-half + place-half, kept independent in the flat lists
        for (old_utxo, new_params) in updates {
            // Cancel half
            let cancel_pays = self.build_cancel_order(&[old_utxo.clone()], return_address)?;
            for pay in cancel_pays {
                if pay.spend_utxos.is_empty() {
                    return Err(anyhow!(
                        "build_bulk_orders: build_cancel_order returned a PayToAddress with empty spend_utxos for cancel of {}; expected at least one script input",
                        old_utxo.tx_hash
                    ));
                }
                for spend in pay.spend_utxos {
                    all_cancels.push(spend);
                }
            }
            // Place half
            let pays = self.build_swap_order(pool_for_swaps()?, new_params)?;
            if pays.is_empty() {
                anyhow::bail!("build_swap_order returned empty list for a bulk update item");
            }
            for p in &pays {
                if !p.spend_utxos.is_empty() {
                    return Err(anyhow!(
                        "build_bulk_orders: build_swap_order returned a PayToAddress with non-empty spend_utxos ({} entries); the bulk path assumes new orders carry no script inputs of their own — DEX impls that need co-inputs must override build_bulk_orders",
                        p.spend_utxos.len()
                    ));
                }
            }
            all_new_orders.extend(pays);
        }

        Ok(BulkOrderPlan {
            cancels: all_cancels,
            new_orders: all_new_orders,
            return_address: return_address.to_string(),
        })
    }

    /// Build a single-tx update of an existing order: spend the old order with
    /// the cancel redeemer AND create a new order at the same script address
    /// with `new_swap_params`. Default impl composes `build_swap_order` and
    /// `build_cancel_order`. DEXes whose "update" needs custom redeemer logic
    /// can override this method.
    ///
    /// The cancelled funds return to `new_swap_params.sender` (the order owner),
    /// not `receiver`. The new order's swap-out is paid to `receiver` per the
    /// usual swap semantics.
    fn build_update_order(
        &self,
        pool: &LiquidityPool,
        old_order_utxos: &[Utxo],
        new_swap_params: &SwapParams,
    ) -> Result<Vec<PayToAddress>> {
        let mut new_order = self.build_swap_order(pool, new_swap_params)?;
        let cancel = self.build_cancel_order(old_order_utxos, &new_swap_params.sender.bech32)?;
        if new_order.is_empty() {
            anyhow::bail!("build_swap_order returned empty PayToAddress list");
        }
        if new_order.len() != 1 {
            anyhow::bail!(
                "build_update_order expects build_swap_order to return exactly 1 PayToAddress; got {}",
                new_order.len()
            );
        }
        if cancel.is_empty() || cancel[0].spend_utxos.is_empty() {
            anyhow::bail!("build_cancel_order returned no spend_utxos");
        }
        if cancel.len() != 1 {
            anyhow::bail!(
                "build_update_order expects build_cancel_order to return exactly 1 PayToAddress (single-order cancel); got {}",
                cancel.len()
            );
        }
        new_order[0].spend_utxos = cancel[0].spend_utxos.clone();
        Ok(new_order)
    }
}
