use chrono::{DateTime, Utc};
use ton_block::{MsgAddrStd, MsgAddressInt};

use relay_eth::ws::H256;
use relay_ton::prelude::{serde_cells, serde_int_addr, serde_std_addr, Cell};

use super::prelude::*;
use crate::engine::bridge::models::EventVote;

pub mod buf_to_hex {
    use serde::{Deserialize, Deserializer, Serializer};

    /// Serializes `buffer` to a lowercase hex string.
    pub fn serialize<T, S>(buffer: &T, serializer: S) -> Result<S::Ok, S::Error>
    where
        T: AsRef<[u8]> + ?Sized,
        S: Serializer,
    {
        serializer.serialize_str(&*hex::encode(&buffer.as_ref()))
    }

    /// Deserializes a lowercase hex string to a `Vec<u8>`.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error;
        String::deserialize(deserializer)
            .and_then(|string| hex::decode(string).map_err(|e| D::Error::custom(e.to_string())))
    }
}

pub mod h256_to_hex {
    use ethereum_types::H256;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<T, S>(buffer: &T, serializer: S) -> Result<S::Ok, S::Error>
    where
        T: AsRef<[u8]> + ?Sized,
        S: Serializer,
    {
        serializer.serialize_str(&*hex::encode(&buffer.as_ref()))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<H256, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error;

        String::deserialize(deserializer).and_then(|string| {
            hex::decode(string)
                .map_err(|e| D::Error::custom(e.to_string()))
                .map(|x| H256::from_slice(&*x))
        })
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct EthTonConfirmationData {
    pub event_transaction: H256,
    pub event_index: u64,
    #[serde(with = "serde_cells")]
    pub event_data: Cell,
    pub event_block_number: u64,
    pub event_block: H256,
    #[serde(with = "serde_int_addr")]
    pub ethereum_event_configuration_address: MsgAddressInt,
}

#[derive(Serialize, Deserialize, Clone)]
pub enum EthTonTransaction {
    Confirm(EthTonConfirmationData),
    Reject(EthTonConfirmationData),
}

impl EthTonTransaction {
    pub fn inner(&self) -> &EthTonConfirmationData {
        match self {
            EthTonTransaction::Confirm(data) => data,
            EthTonTransaction::Reject(data) => data,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub struct TxStat {
    #[serde(with = "h256_to_hex")]
    pub tx_hash: H256,
    pub met: DateTime<Utc>,
    #[serde(with = "serde_std_addr")]
    pub event_addr: MsgAddrStd,
    pub vote: EventVote,
}