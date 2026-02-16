use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Unit {
    pub unit: String,
    #[serde(deserialize_with = "deserialize_quantity")]
    pub quantity: String,
}

fn deserialize_quantity<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Q {
        Str(String),
        Num(u64),
    }
    let q = Q::deserialize(deserializer)?;
    Ok(match q {
        Q::Str(s) => s,
        Q::Num(n) => n.to_string(),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Utxo {
    pub address: String,
    pub tx_hash: String,
    pub tx_index: u32,
    pub output_index: u32,
    pub amount: Vec<Unit>,
    pub block: String,
    pub data_hash: Option<String>,
    pub inline_datum: Option<String>,
    pub reference_script_hash: Option<String>,
}

impl Utxo {
    pub fn get_asset(&self, unit: &str) -> Option<&Unit> {
        self.amount.iter().find(|u| u.unit == unit)
    }

    pub fn has_data_hash(&self) -> bool {
        self.data_hash.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KupoUtxoResponse {
    pub address: String,
    #[serde(rename = "transaction_id")]
    pub tx_id: String,
    #[serde(rename = "output_index", deserialize_with = "deserialize_output_index")]
    pub output_idx: u32,
    pub value: KupoValue,
    #[serde(rename = "created_at")]
    pub created_at: KupoCreatedAt,
    #[serde(rename = "datum_hash")]
    pub datum_hash: Option<String>,
    #[serde(rename = "script_hash")]
    pub script_hash: Option<String>,
}

fn deserialize_output_index<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum O {
        Num(u32),
        Str(String),
    }
    let o = O::deserialize(deserializer)?;
    match o {
        O::Num(n) => Ok(n),
        O::Str(s) => s.parse().map_err(serde::de::Error::custom),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KupoValue {
    #[serde(deserialize_with = "deserialize_coins")]
    pub coins: String,
    pub assets: Option<std::collections::HashMap<String, String>>,
}

fn deserialize_coins<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum C {
        Str(String),
        Num(u64),
    }
    let c = C::deserialize(deserializer)?;
    Ok(match c {
        C::Str(s) => s,
        C::Num(n) => n.to_string(),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KupoCreatedAt {
    #[serde(rename = "header_hash")]
    pub header_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KupoDatumResponse {
    pub datum: String,
}
