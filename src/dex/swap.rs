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
}
