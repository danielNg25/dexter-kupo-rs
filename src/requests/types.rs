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

/// On-chain reference to a UTxO that carries a Plutus script as its
/// reference_script (CIP-33). The executor can attach this UTxO as a
/// `reference_input` instead of including the script bytes in the witness set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UtxoRef {
    pub tx_hash: String,
    pub output_index: u64,
    /// The Blake2b-224 hash of the script at this UTxO. The executor verifies
    /// the reference matches before using it.
    pub script_hash: String,
    /// Size of the referenced script in bytes. Conway era charges a per-byte
    /// fee for reference scripts (`min_fee_ref_script_cost_per_byte`); the
    /// executor multiplies this size by the protocol-param coins-per-byte
    /// to compute the ref-script fee component.
    pub script_size_bytes: u64,
}

/// A UTxO the order intends to spend (input).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpendUtxo {
    pub utxo: Utxo,
    pub redeemer: Option<String>,         // CBOR hex
    /// Inline validator (full script CBOR). Use this when no reference UTxO
    /// is available. Either `validator` or `validator_reference` should be
    /// populated; if both are Some, executors should prefer `validator_reference`.
    pub validator: Option<PlutusScript>,
    /// On-chain script reference (CIP-33). When Some, executors should attach
    /// this UTxO as a `reference_input` and skip the inline script.
    pub validator_reference: Option<UtxoRef>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utxo_ref_serde_round_trips() {
        let r = UtxoRef {
            tx_hash: "cf4ecddde0d81f9ce8fcc881a85eb1f8ccdaf6807f03fea4cd02da896a621776".into(),
            output_index: 0,
            script_hash: "c3e28c36c3447315ba5a56f33da6a6ddc1770a876a8d9f0cb3a97c4c".into(),
            script_size_bytes: 2659,
        };
        let json = serde_json::to_string(&r).expect("serialize");
        let back: UtxoRef = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(r, back);
    }

    #[test]
    fn spend_utxo_with_validator_reference_serde_round_trips() {
        let s = SpendUtxo {
            utxo: Utxo {
                address: "addr1".into(),
                tx_hash: "aa".repeat(32),
                tx_index: 0,
                output_index: 0,
                amount: vec![],
                block: String::new(),
                data_hash: None,
                inline_datum: None,
                reference_script_hash: None,
                datum_type: None,
            },
            redeemer: Some("d87a80".into()),
            validator: None,
            validator_reference: Some(UtxoRef {
                tx_hash: "cf4ecddde0d81f9ce8fcc881a85eb1f8ccdaf6807f03fea4cd02da896a621776".into(),
                output_index: 0,
                script_hash: "c3e28c36c3447315ba5a56f33da6a6ddc1770a876a8d9f0cb3a97c4c".into(),
                script_size_bytes: 2659,
            }),
            signer: Some("addr1".into()),
        };
        let json = serde_json::to_string(&s).expect("serialize");
        let back: SpendUtxo = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(s.validator_reference, back.validator_reference);
    }
}
