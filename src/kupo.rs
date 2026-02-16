use anyhow::Result;
use serde::de::DeserializeOwned;
use crate::models::Utxo;

pub struct KupoApi {
    api_url: String,
    client: reqwest::Client,
}

impl KupoApi {
    pub fn new(api_url: &str) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300)) // 5 minutes for large queries
            .build()
            .expect("Failed to build HTTP client");
        Self {
            api_url: crate::utils::remove_trailing_slash(api_url),
            client,
        }
    }

    pub fn api_url(&self) -> &str {
        &self.api_url
    }

    pub fn with_client(api_url: &str, client: reqwest::Client) -> Self {
        Self {
            api_url: crate::utils::remove_trailing_slash(api_url),
            client,
        }
    }

    fn build_matches_url(&self, match_pattern: &str, unspent: bool) -> String {
        let base = format!("{}/matches/{}", self.api_url, match_pattern);
        if unspent {
            format!("{}?unspent", base)
        } else {
            base
        }
    }

    fn build_datum_url(&self, hash: &str) -> String {
        format!("{}/datums/{}", self.api_url, hash)
    }

    async fn fetch_utxos(&self, match_pattern: &str, unspent: bool) -> Result<Vec<serde_json::Value>> {
        let url = self.build_matches_url(match_pattern, unspent);
        let response = self.client.get(&url).send().await?;
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(anyhow::anyhow!("rate_limited"));
        }
        let body = response.text().await?;
        let parsed: serde_json::Value = serde_json::from_str(&body)?;
        if let Some(arr) = parsed.as_array() {
            Ok(arr.clone())
        } else {
            Ok(vec![parsed])
        }
    }

    async fn fetch_datum(&self, hash: &str) -> Result<serde_json::Value> {
        let url = self.build_datum_url(hash);
        let response = self.client.get(&url).send().await?;
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(anyhow::anyhow!("rate_limited"));
        }
        let body = response.text().await?;
        let parsed: serde_json::Value = serde_json::from_str(&body)?;
        Ok(parsed)
    }

    pub async fn get(&self, match_pattern: &str, unspent: bool) -> Result<Vec<Utxo>> {
        crate::utils::retry(10, 1000, || async {
            let values = self.fetch_utxos(match_pattern, unspent).await?;
            
            let mut utxos = Vec::new();
            
            for v in values {
                let address = v.get("address").and_then(|a| a.as_str()).unwrap_or_default().to_string();
                let tx_id = v.get("transaction_id").and_then(|a| a.as_str()).unwrap_or_default().to_string();
                let output_idx = v.get("output_index").and_then(|a| a.as_u64()).unwrap_or(0) as u32;
                let header_hash = v.get("created_at")
                    .and_then(|c| c.get("header_hash"))
                    .and_then(|h| h.as_str())
                    .unwrap_or_default()
                    .to_string();
                let datum_hash = v.get("datum_hash").and_then(|d| d.as_str()).map(String::from);
                let script_hash = v.get("script_hash").and_then(|s| s.as_str()).map(String::from);

                let value = v.get("value").unwrap_or(&serde_json::Value::Null);
                
                let coins = value.get("coins")
                    .map(|c| {
                        if let Some(s) = c.as_str() {
                            s.to_string()
                        } else if let Some(n) = c.as_u64() {
                            n.to_string()
                        } else {
                            "0".to_string()
                        }
                    })
                    .unwrap_or_else(|| "0".to_string());

                let mut amount = vec![crate::models::Unit {
                    unit: "lovelace".to_string(),
                    quantity: coins,
                }];

                if let Some(assets) = value.get("assets").and_then(|a| a.as_object()) {
                    for (unit, qty) in assets {
                        let qty_str = if let Some(s) = qty.as_str() {
                            s.to_string()
                        } else if let Some(n) = qty.as_u64() {
                            n.to_string()
                        } else {
                            "0".to_string()
                        };
                        amount.push(crate::models::Unit {
                            unit: unit.replace('.', ""),
                            quantity: qty_str,
                        });
                    }
                }

                utxos.push(Utxo {
                    address,
                    tx_hash: tx_id,
                    tx_index: output_idx,
                    output_index: output_idx,
                    amount,
                    block: header_hash,
                    data_hash: datum_hash,
                    inline_datum: None,
                    reference_script_hash: script_hash,
                });
            }
            
            Ok(utxos)
        })
        .await
    }

    pub async fn datum(&self, hash: &str) -> Result<String> {
        crate::utils::retry(10, 1000, || async {
            let response = self.fetch_datum(hash).await?;
            let datum = response.get("datum")
                .and_then(|d| d.as_str())
                .ok_or_else(|| anyhow::anyhow!("No datum found"))?;
            Ok(datum.to_string())
        })
        .await
    }
}
