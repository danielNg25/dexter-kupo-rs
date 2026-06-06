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
    let (hrp, data) = bech32::decode(addr)
        .map_err(|e| anyhow!("bech32 decode failed for `{}`: {}", addr, e))?;
    if hrp.as_str() != "addr" {
        return Err(anyhow!(
            "expected mainnet address with HRP `addr`, got `{}` (testnet is not supported)",
            hrp.as_str()
        ));
    }
    if data.is_empty() {
        return Err(anyhow!("address has no payload"));
    }
    let header = data[0];
    let net_id = header & 0x0f;
    let addr_type = header >> 4;
    if net_id != 1 {
        return Err(anyhow!("expected mainnet (network id 1), got network id {}", net_id));
    }
    // Accept type 0 (key payment, key stake) and type 1 (script payment, key stake).
    if addr_type != 0 && addr_type != 1 {
        return Err(anyhow!(
            "unsupported address type {} (only base addresses 0 and 1 are supported)",
            addr_type
        ));
    }
    let body = &data[1..];
    if body.len() != 56 {
        return Err(anyhow!(
            "base address payload must be 56 bytes (payment + stake), got {}",
            body.len()
        ));
    }
    Ok(WalletAddress {
        payment_key_hash: hex::encode(&body[..28]),
        staking_key_hash: Some(hex::encode(&body[28..])),
        bech32: addr.to_string(),
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    const SENDER_ADDR: &str = "addr1qyfd4vf3pwalnfxucjut2xx653s9ukguwnlrnjjq4qvld76r7sjjnw3fd7elhw73fqtcjae3yxd9xwwn2x265mnadv3qyun95l";
    const SENDER_PKH:  &str = "12dab1310bbbf9a4dcc4b8b518daa4605e591c74fe39ca40a819f6fb";
    const SENDER_STAKE_KH: &str = "43f42529ba296fb3fbbbd14817897731219a5339d35195aa6e7d6b22";

    #[test]
    fn decode_known_mainnet_sender_address() {
        let w = decode_base_address(SENDER_ADDR).unwrap();
        assert_eq!(w.payment_key_hash, SENDER_PKH);
        assert_eq!(w.staking_key_hash.as_deref(), Some(SENDER_STAKE_KH));
        assert_eq!(w.bech32, SENDER_ADDR);
    }

    #[test]
    fn decode_rejects_non_addr1() {
        assert!(decode_base_address("stake1xyz").is_err());
        assert!(decode_base_address("not_an_address").is_err());
    }

    #[test]
    fn decode_rejects_testnet() {
        // Mainnet only (network ID 1). A testnet header (net 0) should be rejected.
        // Synthesize a valid-looking type-0 testnet header for the test.
        // hrp = "addr_test" → reject because we require mainnet "addr".
        let testnet = "addr_test1qpu2pq49jsjz5n4dsec5x2j8gx5tn5lp0c5l07x8t0t8ydqkvfvjs7tva8ytkj0dfllt6rze4kzg7ds20j5xqa7yz0sq2yvnz9";
        assert!(decode_base_address(testnet).is_err());
    }
}
