//! `DexSwap` — the write-side trait (order construction).
//!
//! Kept separate from `BaseDex` (reading) so the reading and writing surfaces
//! don't entangle. A DEX impl can implement both.

use anyhow::Result;

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
