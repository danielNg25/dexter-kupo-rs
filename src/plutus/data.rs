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
        match self {
            PlutusData::Int(i) => write_int(*i, out),
            PlutusData::Bytes(b) => {
                write_head(2, b.len() as u64, out);
                out.extend_from_slice(b);
                Ok(())
            }
            PlutusData::Constr(idx, fields) => {
                let idx = *idx;
                if idx <= 6 {
                    write_tag(121 + idx, out);
                    write_fields(fields, out)?;
                } else if idx <= 127 {
                    write_tag(1280 + (idx - 7), out);
                    write_fields(fields, out)?;
                } else {
                    // tag 102, value = definite array [Int(idx), array_of_fields]
                    write_tag(102, out);
                    out.push(0x82); // array of 2
                    write_int(idx as i128, out)?;
                    // The inner array_of_fields uses the same indef/def rule as a Constr's fields.
                    write_fields(fields, out)?;
                }
                Ok(())
            }
            PlutusData::List(items) => write_fields(items, out),
        }
    }
}

/// Write a CBOR head byte for major type `mt` (0..=7) with unsigned argument `n`.
/// Picks the smallest width (immediate / u8 / u16 / u32 / u64).
fn write_head(mt: u8, n: u64, out: &mut Vec<u8>) {
    debug_assert!(mt < 8);
    let high = mt << 5;
    if n < 24 {
        out.push(high | (n as u8));
    } else if n <= u8::MAX as u64 {
        out.push(high | 24);
        out.push(n as u8);
    } else if n <= u16::MAX as u64 {
        out.push(high | 25);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else if n <= u32::MAX as u64 {
        out.push(high | 26);
        out.extend_from_slice(&(n as u32).to_be_bytes());
    } else {
        out.push(high | 27);
        out.extend_from_slice(&n.to_be_bytes());
    }
}

fn write_int(i: i128, out: &mut Vec<u8>) -> Result<()> {
    if i >= 0 {
        let n: u64 = i.try_into().map_err(|_| anyhow!("Int {} too large for CBOR uint", i))?;
        write_head(0, n, out);
    } else {
        // CBOR negative: major type 1, argument = -1 - i
        let arg_i128 = -1 - i;
        let n: u64 = arg_i128.try_into().map_err(|_| anyhow!("Int {} too small for CBOR nint", i))?;
        write_head(1, n, out);
    }
    Ok(())
}

/// CBOR tag head (major type 6).
fn write_tag(tag: u64, out: &mut Vec<u8>) {
    write_head(6, tag, out);
}

/// Write a fields array using Lucid's convention:
///   * empty   → definite-length 0 array `80`
///   * non-empty → indefinite-length array `9f ... ff`
fn write_fields(fields: &[PlutusData], out: &mut Vec<u8>) -> Result<()> {
    if fields.is_empty() {
        out.push(0x80); // definite array, length 0
    } else {
        out.push(0x9f); // indefinite array start
        for f in fields {
            f.write_cbor(out)?;
        }
        out.push(0xff); // break
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cbor(d: PlutusData) -> String {
        d.to_cbor_hex().unwrap()
    }

    #[test]
    fn int_small_uint() {
        assert_eq!(cbor(PlutusData::Int(0)),   "00");
        assert_eq!(cbor(PlutusData::Int(23)),  "17");
        assert_eq!(cbor(PlutusData::Int(24)),  "1818");
        assert_eq!(cbor(PlutusData::Int(255)), "18ff");
        assert_eq!(cbor(PlutusData::Int(256)), "190100");
    }

    #[test]
    fn int_larger_uints() {
        // 2_000_000_000 fits in u32: 0x77359400
        assert_eq!(cbor(PlutusData::Int(2_000_000_000)), "1a77359400");
        // 341_880_341 fits in u32: 0x1460ae15
        assert_eq!(cbor(PlutusData::Int(341_880_341)),  "1a1460ae15");
        // 2_000_000 fits in u32: 0x001e8480 — but CBOR picks the smallest width
        assert_eq!(cbor(PlutusData::Int(2_000_000)),    "1a001e8480");
    }

    #[test]
    fn int_negative() {
        assert_eq!(cbor(PlutusData::Int(-1)),  "20");
        assert_eq!(cbor(PlutusData::Int(-24)), "37");
        assert_eq!(cbor(PlutusData::Int(-25)), "3818");
    }

    #[test]
    fn bytes_short_and_28() {
        assert_eq!(cbor(PlutusData::Bytes(vec![])),           "40");
        assert_eq!(cbor(PlutusData::Bytes(vec![1,2,3])),      "43010203");
        // 28-byte key hash (matches the on-chain golden's sender pkh prefix)
        let pkh = hex::decode("e6f174b44628064bfdc62cdf2be63c50b2cd68625efe6847f5fdbf83").unwrap();
        assert_eq!(cbor(PlutusData::Bytes(pkh)),
            "581ce6f174b44628064bfdc62cdf2be63c50b2cd68625efe6847f5fdbf83");
    }

    #[test]
    fn bytes_32() {
        // 32-byte LP token name from the golden
        let name = hex::decode("7dd6988c5a86693c76aeec1ea94afa41770be0de21a775ca7a2a1eabdb6a0171").unwrap();
        assert_eq!(cbor(PlutusData::Bytes(name)),
            "58207dd6988c5a86693c76aeec1ea94afa41770be0de21a775ca7a2a1eabdb6a0171");
    }

    #[test]
    fn constr_empty_uses_definite_empty_array() {
        // Lucid emits empty fields as `80` (definite empty array), NOT `9fff`.
        // This matches the on-chain golden (refund_receiver_datum, success_receiver_datum, killable=False).
        assert_eq!(cbor(PlutusData::Constr(0, vec![])), "d87980"); // tag 121, []
        assert_eq!(cbor(PlutusData::Constr(1, vec![])), "d87a80"); // tag 122, []
        assert_eq!(cbor(PlutusData::Constr(6, vec![])), "d87f80"); // tag 127, []
    }

    #[test]
    fn constr_nonempty_uses_indefinite_array() {
        // tag 121 (Constr 0), 1 field, indefinite array: d8 79 9f <field> ff
        assert_eq!(
            cbor(PlutusData::Constr(0, vec![PlutusData::Int(0)])),
            "d8799f00ff"
        );
    }

    #[test]
    fn constr_index_7_to_127_uses_tag_1280_plus_offset() {
        // index 7 → tag 1280 = 0x0500 → head: d9 05 00
        assert_eq!(cbor(PlutusData::Constr(7,   vec![])), "d9050080");
        // index 127 → tag 1280 + (127-7) = 1400 = 0x0578
        assert_eq!(cbor(PlutusData::Constr(127, vec![])), "d9057880");
    }

    #[test]
    fn constr_index_above_127_uses_tag_102() {
        // d8 66 = tag(102); 82 = array(2); 18 80 = Int(128); 80 = array(0)
        assert_eq!(cbor(PlutusData::Constr(128, vec![])), "d86682188080");
    }

    #[test]
    fn list_empty_and_nonempty() {
        assert_eq!(cbor(PlutusData::List(vec![])), "80");
        assert_eq!(
            cbor(PlutusData::List(vec![PlutusData::Int(1), PlutusData::Int(2)])),
            "9f0102ff"
        );
    }
}
