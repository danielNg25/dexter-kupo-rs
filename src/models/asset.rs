use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Asset {
    pub policy_id: String,
    pub name_hex: String,
    pub decimals: u8,
}

impl Asset {
    pub fn new(policy_id: &str, name_hex: &str, decimals: u8) -> Self {
        Self {
            policy_id: policy_id.to_string(),
            name_hex: name_hex.to_string(),
            decimals,
        }
    }

    pub fn from_identifier(id: &str, decimals: u8) -> Asset {
        let id = id.replace('.', "");
        Asset::new(&id[..56], &id[56..], decimals)
    }

    pub fn identifier(&self, delimiter: &str) -> String {
        format!("{}{}{}", self.policy_id, delimiter, self.name_hex)
    }

    pub fn asset_name(&self) -> String {
        String::from_utf8_lossy(&hex::decode(&self.name_hex).unwrap_or_default()).to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Token {
    Lovelace,
    Asset(Asset),
}

impl Token {
    pub fn is_lovelace(&self) -> bool {
        matches!(self, Token::Lovelace)
    }

    pub fn policy_id(&self) -> Option<&str> {
        match self {
            Token::Lovelace => None,
            Token::Asset(a) => Some(&a.policy_id),
        }
    }

    pub fn as_asset(&self) -> Option<&Asset> {
        match self {
            Token::Lovelace => None,
            Token::Asset(a) => Some(a),
        }
    }
}

pub fn from_identifier(id: &str, decimals: u8) -> Token {
    let id = id.replace('.', "");
    if id == "lovelace" || id.is_empty() {
        return Token::Lovelace;
    }
    if id.len() < 56 {
        return Token::Lovelace;
    }
    Token::Asset(Asset::from_identifier(&id, decimals))
}

pub fn token_name(token: &Token) -> String {
    match token {
        Token::Lovelace => "ADA".to_string(),
        Token::Asset(a) => a.asset_name(),
    }
}

pub fn token_identifier(token: &Token) -> String {
    match token {
        Token::Lovelace => "lovelace".to_string(),
        Token::Asset(a) => a.identifier(""),
    }
}
