//! Fluent builder for a Minswap V2 cancel order.

use anyhow::{anyhow, Result};

use crate::dex::DexSwap;
use crate::models::Utxo;
use crate::requests::types::PayToAddress;

pub struct CancelSwapRequest<'a, D: DexSwap + ?Sized> {
    dex: &'a D,
    utxos: Vec<Utxo>,
    return_address: Option<String>,
}

impl<'a, D: DexSwap + ?Sized> CancelSwapRequest<'a, D> {
    pub fn new(dex: &'a D) -> Self {
        Self { dex, utxos: vec![], return_address: None }
    }
    pub fn with_order_utxos(mut self, utxos: Vec<Utxo>) -> Self {
        self.utxos = utxos; self
    }
    pub fn with_return_address(mut self, addr: &str) -> Self {
        self.return_address = Some(addr.to_string()); self
    }
    pub fn build(self) -> Result<Vec<PayToAddress>> {
        if self.utxos.is_empty() {
            return Err(anyhow!("no order UTxOs supplied"));
        }
        let addr = self.return_address.ok_or_else(|| anyhow!("return address not set"))?;
        self.dex.build_cancel_order(&self.utxos, &addr)
    }
}
