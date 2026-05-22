use alloy_primitives::{Address, B256, U256};
use serde::{Deserialize, Serialize};

/// QuickNode-compatible transfer row. Field set matches the existing
/// `apps/tasks` controller exactly (hex quantity strings, decimal `value`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Transfer {
    /// Token contract address. Always the configured native marker
    /// (`0x0000...0000`) for XPL.
    pub address: Address,
    #[serde(rename = "blockHash")]
    pub block_hash: B256,
    #[serde(rename = "blockNumber")]
    pub block_number: HexQuantity,
    pub from: Address,
    #[serde(rename = "logIndex")]
    pub log_index: HexQuantity,
    pub name: TransferName,
    pub to: Address,
    #[serde(rename = "transactionHash")]
    pub transaction_hash: B256,
    pub value: DecimalU256,
}

/// Wrapper carrying the controller's outer shape: `{ "result": Transfer[] }`.
#[derive(Debug, Clone, Serialize)]
pub struct WebhookBody<'a> {
    pub result: &'a [Transfer],
}

/// Discriminator emitted in the `name` field.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TransferName {
    #[serde(rename = "XPL")]
    Xpl,
    #[serde(rename = "XPL Fee")]
    Fee,
}

/// `0x`-prefixed hex quantity. Encodes as `"0x0"` for zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HexQuantity(pub u64);

impl<'de> Deserialize<'de> for HexQuantity {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        let stripped = s.strip_prefix("0x").unwrap_or(&s);
        let n = u64::from_str_radix(stripped, 16).map_err(serde::de::Error::custom)?;
        Ok(Self(n))
    }
}

impl From<u64> for HexQuantity {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl Serialize for HexQuantity {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&format_args!("0x{:x}", self.0))
    }
}

/// Decimal-encoded `U256`. The controller expects wei as a base-10 string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecimalU256(pub U256);

impl<'de> Deserialize<'de> for DecimalU256 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        let n = U256::from_str_radix(&s, 10).map_err(serde::de::Error::custom)?;
        Ok(Self(n))
    }
}

impl From<U256> for DecimalU256 {
    fn from(value: U256) -> Self {
        Self(value)
    }
}

impl Serialize for DecimalU256 {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, b256};

    #[test]
    fn transfer_serializes_to_quicknode_shape() {
        let row = Transfer {
            address: Address::ZERO,
            block_hash: b256!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            block_number: 123.into(),
            from: address!("0x1111111111111111111111111111111111111111"),
            log_index: 0.into(),
            name: TransferName::Xpl,
            to: address!("0x2222222222222222222222222222222222222222"),
            transaction_hash: b256!(
                "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            ),
            value: U256::from(1_000_000_000_000_000_000_u128).into(),
        };

        let body = WebhookBody { result: std::slice::from_ref(&row) };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&body).unwrap()).expect("body parses back");

        let row_json = &json["result"][0];
        assert_eq!(row_json["address"], "0x0000000000000000000000000000000000000000");
        assert_eq!(row_json["blockNumber"], "0x7b");
        assert_eq!(row_json["logIndex"], "0x0");
        assert_eq!(row_json["name"], "XPL");
        assert_eq!(row_json["value"], "1000000000000000000");
        assert_eq!(row_json["from"], "0x1111111111111111111111111111111111111111");
        assert_eq!(row_json["to"], "0x2222222222222222222222222222222222222222");
    }

    #[test]
    fn fee_row_uses_xpl_fee_name() {
        let row = Transfer {
            address: Address::ZERO,
            block_hash: B256::ZERO,
            block_number: 1.into(),
            from: Address::ZERO,
            log_index: 5.into(),
            name: TransferName::Fee,
            to: Address::ZERO,
            transaction_hash: B256::ZERO,
            value: U256::from(21_000).into(),
        };
        let s = serde_json::to_string(&row).unwrap();
        assert!(s.contains("\"name\":\"XPL Fee\""));
        assert!(s.contains("\"logIndex\":\"0x5\""));
        assert!(s.contains("\"value\":\"21000\""));
    }
}
