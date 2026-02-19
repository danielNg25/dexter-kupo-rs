use anyhow::{anyhow, Result};

pub fn join_policy_id(policy_id: &str) -> String {
    policy_id.replace('.', "")
}

pub fn split_policy_id(policy_id: &str) -> String {
    if policy_id.len() == 56 {
        format!("{}.{}", &policy_id[..56], &policy_id[56..])
    } else {
        policy_id.to_string()
    }
}

pub fn is_shelly_address(address: &str) -> bool {
    address.starts_with("addr1") || address.starts_with("stake1")
}

pub fn remove_trailing_slash(url: &str) -> String {
    if url.ends_with('/') {
        url[..url.len() - 1].to_string()
    } else {
        url.to_string()
    }
}

/// Convert a Plutus script hash (28 bytes hex) to a mainnet enterprise script address (bech32 `addr1w...`).
///
/// Cardano enterprise script address format:
///   - Header byte: 0x71 (type 7 = script credential, no staking, network 1 = mainnet)
///   - Payload: 28-byte script hash
///   - Encoded as bech32 with HRP "addr"
pub fn script_hash_to_address(script_hash_hex: &str) -> Result<String> {
    let hash_bytes = hex::decode(script_hash_hex)
        .map_err(|e| anyhow!("invalid script hash hex: {}", e))?;
    if hash_bytes.len() != 28 {
        return Err(anyhow!(
            "script hash must be 28 bytes, got {}",
            hash_bytes.len()
        ));
    }
    // Header byte 0x71 = 0b0111_0001:
    //   upper nibble 0111 = type 7 (script credential, no staking)
    //   lower nibble 0001 = network ID 1 (mainnet)
    let mut payload = Vec::with_capacity(29);
    payload.push(0x71);
    payload.extend_from_slice(&hash_bytes);
    let hrp = bech32::Hrp::parse("addr").map_err(|e| anyhow!("bech32 HRP error: {}", e))?;
    bech32::encode::<bech32::Bech32>(hrp, &payload)
        .map_err(|e| anyhow!("bech32 encode error: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_script_hash_to_address_known_addresses() {
        // These are the two known ChadSwap order addresses and their script hashes
        // from the ChadSwap API.
        let addr1 = script_hash_to_address(
            "ea07914e72654ca5a5c5e26a95596e6fa0b5c4c317e43e2f92457ea1",
        )
        .unwrap();
        assert_eq!(
            addr1,
            "addr1w84q0y2wwfj5efd9ch3x492edeh6pdwycvt7g030jfzhagg5ftr54",
            "script hash ea07... should map to addr1w84q..."
        );

        let addr2 = script_hash_to_address(
            "8c66f1ac8b57debcab9a07b3b9b0a5595a5ad76e3bcae756c82efe4f",
        )
        .unwrap();
        assert_eq!(
            addr2,
            "addr1wxxxdudv3dtaa09tngrm8wds54v45kkhdcau4e6keqh0uncksc7pn",
            "script hash 8c66... should map to addr1wxxx..."
        );
    }

    #[test]
    fn test_script_hash_to_address_invalid_length() {
        assert!(script_hash_to_address("abcd").is_err());
    }

    #[test]
    fn test_script_hash_to_address_invalid_hex() {
        assert!(script_hash_to_address("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz").is_err());
    }
}

pub async fn retry<T, E, F, Fut>(mut retries: u32, base_delay_ms: u64, mut f: F) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Debug,
{
    let mut attempt = 0u32;
    loop {
        match f().await {
            Ok(result) => return Ok(result),
            Err(e) if retries == 0 => return Err(e),
            Err(e) => {
                // Exponential backoff: base_delay * 2^attempt, capped at 30s
                let delay = (base_delay_ms * (1u64 << attempt.min(5))).min(30_000);
                eprintln!("[retry] attempt {} failed ({:?}), retrying in {}ms...", attempt + 1, e, delay);
                tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
                retries -= 1;
                attempt += 1;
            }
        }
    }
}
