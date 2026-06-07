//! Boundary objects handed to a Phase-2 tx layer (or any consumer).
//! Mirrors dexter's `PayToAddress` shape.

use crate::address::WalletAddress;
use crate::models::{Token, Utxo};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AddressType {
    Contract,
    Base,
    Enterprise,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PlutusVersion {
    V1,
    V2,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum OrderKind {
    Market,
    Limit,
}

/// One asset amount in an output (lovelace or a native asset).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AssetAmount {
    /// `"lovelace"` for ADA, or `<policy_id_hex><asset_name_hex>` for native assets.
    pub unit: String,
    pub quantity: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlutusScript {
    pub version: PlutusVersion,
    pub cbor_hex: String,
}

/// A UTxO the order intends to spend (input).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpendUtxo {
    pub utxo: Utxo,
    pub redeemer: Option<String>,         // CBOR hex
    pub validator: Option<PlutusScript>,
    pub signer: Option<String>,
}

/// Instruction for one transaction output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayToAddress {
    pub address: String,
    pub address_type: AddressType,
    pub assets: Vec<AssetAmount>,
    pub datum: Option<String>,           // CBOR hex
    pub is_inline_datum: bool,
    pub spend_utxos: Vec<SpendUtxo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapFee {
    pub id: String,
    pub title: String,
    pub description: String,
    pub value: u64,
    pub is_returned: bool,
}

/// Resolved parameters consumed by `DexSwap::build_swap_order`.
/// `min_receive` is already in raw on-chain units — the builder never sees
/// human price or decimals.
#[derive(Debug, Clone)]
pub struct SwapParams {
    pub sender: WalletAddress,
    pub receiver: WalletAddress,
    pub swap_in_token: Token,
    pub swap_out_token: Token,
    pub swap_in_amount: u64,
    pub min_receive: u64,
    pub kind: OrderKind,
    pub kill_on_failed: bool,
    pub spend_utxos: Vec<Utxo>,
}
