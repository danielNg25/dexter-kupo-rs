//! Plutus data encoding (CBOR).
//!
//! The decode-side counterpart lives in `dex/cbor.rs`. This module is the
//! encode side — used to build order datums for DEX interaction.

pub mod data;
pub use data::PlutusData;
