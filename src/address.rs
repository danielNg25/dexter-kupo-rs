//! Mainnet base address decode + V2 order address derivation.
//!
//! Cardano Shelley address layout (header byte | payload):
//!   * type 0 (base, key payment + key stake)    header = 0x01 (net 1 = mainnet)
//!   * type 1 (base, script payment + key stake) header = 0x11
//! Payload of a base address = payment_credential(28) || stake_credential(28).

use anyhow::{anyhow, Result};

#[derive(Debug, Clone)]
pub struct WalletAddress {
    pub payment_key_hash: String,         // 28-byte hex
    pub staking_key_hash: Option<String>, // 28-byte hex (None for enterprise)
    pub bech32: String,                   // original bech32 the address was parsed from
}

/// Decode a mainnet base address (`addr1q...` or `addr1z...`) into its credentials.
pub fn decode_base_address(addr: &str) -> Result<WalletAddress> {
    unimplemented!("Task 7")
}

/// Build a mainnet base address (header 0x11) from a script payment credential
/// + a key staking credential. Matches dexter's `lucidUtils.credentialToAddress(...)`
/// for the per-user V2 order address.
pub fn script_and_stake_to_base_address(
    script_hash_hex: &str,
    stake_key_hash_hex: &str,
) -> Result<String> {
    unimplemented!("Task 8")
}
