//! Fluent builder for a single Cardano transaction bundling multiple
//! Minswap V2 order actions: fresh places, pure cancels, and atomic
//! cancel+place updates.
//!
//! See spec: `docs/superpowers/specs/2026-06-08-bulk-swap-request-design.md`.

use anyhow::{anyhow, Result};

use crate::dex::DexSwap;
use crate::models::{LiquidityPool, Utxo};
use crate::requests::types::{PayToAddress, SpendUtxo, SwapParams};

/// What the bulk builder hands back. Flat from the caller's perspective:
/// all script inputs in `cancels`, all script outputs in `new_orders`.
/// Pure-cancel, pure-place, and update intents all collapse into this shape.
#[derive(Debug, Clone)]
pub struct BulkOrderPlan {
    pub cancels: Vec<SpendUtxo>,
    pub new_orders: Vec<PayToAddress>,
    pub return_address: String,
}

pub struct BulkSwapRequest<'a, D: DexSwap + ?Sized> {
    dex: &'a D,
    pool: Option<LiquidityPool>,
    return_address: Option<String>,
    cancels: Vec<Utxo>,
    swaps: Vec<SwapParams>,
    updates: Vec<(Utxo, SwapParams)>,
}

impl<'a, D: DexSwap + ?Sized> BulkSwapRequest<'a, D> {
    pub fn new(dex: &'a D) -> Self {
        Self { dex, pool: None, return_address: None, cancels: vec![], swaps: vec![], updates: vec![] }
    }

    pub fn for_pool(mut self, pool: LiquidityPool) -> Self {
        self.pool = Some(pool);
        self
    }

    pub fn with_return_address(mut self, addr: &str) -> Result<Self> {
        self.return_address = Some(addr.to_string());
        Ok(self)
    }

    pub fn add_cancel(mut self, utxo: Utxo) -> Self {
        self.cancels.push(utxo);
        self
    }

    pub fn add_swap(mut self, params: SwapParams) -> Self {
        self.swaps.push(params);
        self
    }

    pub fn add_update(mut self, old_utxo: Utxo, new_params: SwapParams) -> Self {
        self.updates.push((old_utxo, new_params));
        self
    }

    pub fn build(self) -> Result<BulkOrderPlan> {
        let return_address = self.return_address
            .ok_or_else(|| anyhow!("BulkSwapRequest: return_address not set"))?;
        if self.cancels.is_empty() && self.swaps.is_empty() && self.updates.is_empty() {
            return Err(anyhow!("BulkSwapRequest: empty bundle (need at least one action)"));
        }
        // `pool` is required ONLY when the bundle has at least one new order
        // (swap or update). Pure-cancel-only bundles don't need a pool.
        let needs_pool = !self.swaps.is_empty() || !self.updates.is_empty();
        if needs_pool && self.pool.is_none() {
            return Err(anyhow!(
                "BulkSwapRequest: pool not set (call for_pool) — required because the bundle has at least one swap or update"
            ));
        }
        self.dex.build_bulk_orders(
            self.pool.as_ref(),
            &self.cancels,
            &self.swaps,
            &self.updates,
            &return_address,
        )
    }
}
