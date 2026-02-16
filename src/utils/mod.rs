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
