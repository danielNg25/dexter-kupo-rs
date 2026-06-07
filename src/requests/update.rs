//! Fluent builder for a single-tx Minswap order update: spends an existing
//! order and places a new one with different parameters, atomically.

use anyhow::{anyhow, Result};

use crate::dex::DexSwap;
use crate::models::{LiquidityPool, Token, Utxo};
use crate::requests::swap::SwapRequest;
use crate::requests::types::PayToAddress;

pub struct UpdateSwapRequest<'a, D: DexSwap + ?Sized> {
    inner: SwapRequest<'a, D>,
    old_order_utxos: Vec<Utxo>,
}

impl<'a, D: DexSwap + ?Sized> UpdateSwapRequest<'a, D> {
    pub fn new(dex: &'a D) -> Self {
        Self { inner: SwapRequest::new(dex), old_order_utxos: vec![] }
    }

    pub fn with_old_order_utxos(mut self, utxos: Vec<Utxo>) -> Self {
        self.old_order_utxos = utxos;
        self
    }

    // --- Delegate all SwapRequest fluent methods to inner ---

    pub fn for_pool(mut self, pool: LiquidityPool) -> Self {
        self.inner = self.inner.for_pool(pool);
        self
    }

    pub fn with_swap_in_token(mut self, token: Token) -> Self {
        self.inner = self.inner.with_swap_in_token(token);
        self
    }

    pub fn with_swap_in_amount(mut self, amount: u64) -> Self {
        self.inner = self.inner.with_swap_in_amount(amount);
        self
    }

    pub fn with_slippage_percent(mut self, slippage: f64) -> Self {
        self.inner = self.inner.with_slippage_percent(slippage);
        self
    }

    pub fn with_minimum_receive(mut self, raw: u64) -> Self {
        self.inner = self.inner.with_minimum_receive(raw);
        self
    }

    pub fn with_kill_on_failed(mut self, kill: bool) -> Self {
        self.inner = self.inner.with_kill_on_failed(kill);
        self
    }

    pub fn with_sender_address(mut self, bech32: &str) -> Result<Self> {
        self.inner = self.inner.with_sender_address(bech32)?;
        Ok(self)
    }

    pub fn with_receiver_address(mut self, bech32: &str) -> Result<Self> {
        self.inner = self.inner.with_receiver_address(bech32)?;
        Ok(self)
    }

    pub fn with_spend_utxos(mut self, utxos: Vec<Utxo>) -> Self {
        self.inner = self.inner.with_spend_utxos(utxos);
        self
    }

    pub fn build(self) -> Result<Vec<PayToAddress>> {
        if self.old_order_utxos.is_empty() {
            return Err(anyhow!("no old order UTxOs supplied for update"));
        }
        let (dex, pool, params) = self.inner.into_dex_pool_params()?;
        dex.build_update_order(&pool, &self.old_order_utxos, &params)
    }
}
