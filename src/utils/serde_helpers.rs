pub const fn const_usize<const N: usize>() -> usize {
    N
}

pub const fn const_u64<const N: u64>() -> u64 {
    N
}

pub mod serde_commitment {
    use std::str::FromStr;

    use serde::de::Error;
    use serde::Deserialize;
    use solana_sdk::commitment_config::CommitmentConfig;

    pub fn serialize<S>(data: &CommitmentConfig, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&data.commitment.to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<CommitmentConfig, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let data = String::deserialize(deserializer)?;
        CommitmentConfig::from_str(&data).map_err(D::Error::custom)
    }
}

/*pub mod abi_transactions {
    use std::collections::HashMap;

    use nekoton_abi::*;
    use ton_abi::{ParamType, TokenValue};
    use ton_types::UInt256;

    pub fn unpack(value: &TokenValue) -> UnpackerResult<HashMap<UInt256, Vec<u8>>> {
        match value {
            TokenValue::Map(ParamType::Uint(256), _, values) => {
                let mut map = HashMap::new();
                for (key, value) in values {
                    let key =


                    let key = key
                        .parse::<UInt256>()
                        .map_err(|_| UnpackerError::InvalidAbi)?;

                    let value = value.clone().unpack()?;
                    map.insert(key, value);
                }
                Ok(map)
            }
            _ => Err(UnpackerError::InvalidAbi),
        }
    }

    /*pub fn param_type() -> ParamType {
        ParamType::Map(Box::new(param_type_key()), Box::new(param_type_value()))
    }

    pub fn param_type_key() -> ParamType {
        ParamType::Address
    }

    pub fn param_type_value() -> ParamType {
        ParamType::Tuple(vec![
            Param::new("farming_pool", ParamType::Address),
            Param::new("ping_frequency", ParamType::Uint(64)),
            Param::new("last_ping", ParamType::Uint(64)),
            Param::new("ping_counter", ParamType::Uint(64)),
            Param::new("auto_ping_enabled", ParamType::Bool),
        ])
    }*/
}
*/
