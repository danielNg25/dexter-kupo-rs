/// Shared CBOR helpers for Plutus datum parsing (used by all DEX implementations).
use anyhow::{anyhow, Result};
use ciborium::value::Value;

/// Extract the inner field array from a Plutus constructor tag.
/// Plutus constructors are CBOR-tagged values: Tag(121+alt, Array([fields...])).
pub fn constr_fields(v: &Value) -> Result<&Vec<Value>> {
    match v {
        Value::Tag(_, inner) => match inner.as_ref() {
            Value::Array(fields) => Ok(fields),
            _ => Err(anyhow!("Expected array inside constr tag")),
        },
        _ => Err(anyhow!("Expected CBOR tag for constr, got {:?}", v)),
    }
}

/// Return true if a value is a CBOR-tagged constructor (any alternative).
pub fn is_constr(v: &Value) -> bool {
    matches!(v, Value::Tag(_, _))
}

/// Return true if a value is a CBOR-tagged constructor with at least one field.
/// Used to detect WingRidersV2 stable pools: a non-empty constr at field[20].
/// Empty constrs (CBORTag(121, [])) represent a Plutus `Nothing` — NOT a stable pool.
pub fn is_nonempty_constr(v: &Value) -> bool {
    match v {
        Value::Tag(_, inner) => match inner.as_ref() {
            Value::Array(fields) => !fields.is_empty(),
            _ => false,
        },
        _ => false,
    }
}

/// Read a u64 from a ciborium Integer value.
pub fn value_to_u64(v: &Value) -> Result<u64> {
    match v {
        Value::Integer(i) => {
            let n: i128 = (*i).into();
            if n < 0 {
                Err(anyhow!("Negative integer where u64 expected: {}", n))
            } else {
                Ok(n as u64)
            }
        }
        _ => Err(anyhow!("Expected integer, got {:?}", v)),
    }
}

/// Read a possibly-negative i64 from a ciborium Integer value.
pub fn value_to_i64(v: &Value) -> Result<i64> {
    match v {
        Value::Integer(i) => {
            let n: i128 = (*i).into();
            Ok(n as i64)
        }
        _ => Err(anyhow!("Expected integer, got {:?}", v)),
    }
}

/// Read bytes from a ciborium Bytes value and return them as a lowercase hex string.
pub fn value_to_hex(v: &Value) -> Result<String> {
    match v {
        Value::Bytes(b) => Ok(hex::encode(b)),
        _ => Err(anyhow!("Expected bytes, got {:?}", v)),
    }
}

/// Parse the two-field constr that represents a Cardano asset: (policy_bytes, name_bytes).
/// Returns `(policy_hex, name_hex)`.
pub fn parse_asset_constr(v: &Value) -> Result<(String, String)> {
    let fields = constr_fields(v)?;
    if fields.len() != 2 {
        return Err(anyhow!(
            "Asset constr expected 2 fields, got {}",
            fields.len()
        ));
    }
    let policy = value_to_hex(&fields[0])?;
    let name = value_to_hex(&fields[1])?;
    Ok((policy, name))
}

/// Decode a CBOR hex string into a ciborium Value.
pub fn decode_cbor(cbor_hex: &str) -> Result<Value> {
    let bytes = hex::decode(cbor_hex)?;
    ciborium::de::from_reader(bytes.as_slice()).map_err(|e| anyhow!("CBOR decode error: {}", e))
}
