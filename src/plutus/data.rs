//! `PlutusData` — a typed Plutus data tree that serializes to CBOR.
//!
//! Mirrors Lucid's `C.PlutusData`. The Plutus constructor tag rules are:
//!   * index `0..=6`   → CBOR tag `121 + index`
//!   * index `7..=127` → CBOR tag `1280 + (index - 7)`
//!   * index `> 127`   → CBOR tag `102`, value = Array([Int(index), List(fields)])
//!
//! Array fields use indefinite-length CBOR arrays (`9f ... ff`) when non-empty
//! and a definite-length empty array (`80`) when empty — this matches the
//! byte form Lucid (and Minswap) emit, and is what the spec §7.1 golden
//! datum requires.

use anyhow::{anyhow, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum PlutusData {
    Constr(u64, Vec<PlutusData>),
    Int(i128),
    Bytes(Vec<u8>),
    List(Vec<PlutusData>),
}

impl PlutusData {
    /// Convenience: construct `PlutusData::Bytes` from a hex string.
    pub fn bytes_hex(hex_str: &str) -> Result<Self> {
        let bytes = hex::decode(hex_str)
            .map_err(|e| anyhow!("invalid hex for PlutusData::Bytes: {}", e))?;
        Ok(PlutusData::Bytes(bytes))
    }

    /// Serialize this Plutus data tree to a lowercase hex-encoded CBOR string.
    pub fn to_cbor_hex(&self) -> Result<String> {
        let mut buf: Vec<u8> = Vec::new();
        self.write_cbor(&mut buf)?;
        Ok(hex::encode(buf))
    }

    fn write_cbor(&self, out: &mut Vec<u8>) -> Result<()> {
        // implemented in Tasks 2–4
        unimplemented!()
    }
}
