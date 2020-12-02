use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

use serde::ser::SerializeSeq;
use serde::{Deserialize, Serialize, Serializer};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub network_config: NetworkConfig,
    pub network_name: String,
    pub verbosity: u8,
    pub keystore: KeystoreType,
    pub last_block_threshold_sec: u64,
    pub subscription_polling_interval_sec: u64,

    /// seconds
    #[serde(default)]
    pub max_initial_rescan_gap: Option<u32>,
    /// seconds
    #[serde(default)]
    pub max_rescan_gap: Option<u32>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            network_config: default_testnet_config(),
            network_name: "testnet".to_string(),
            verbosity: 4,
            keystore: KeystoreType::InMemory,
            last_block_threshold_sec: 1,
            subscription_polling_interval_sec: 1,
            max_initial_rescan_gap: None,
            max_rescan_gap: None,
        }
    }
}

impl From<Config> for tonlib::Config {
    fn from(c: Config) -> Self {
        Self {
            network_config: serialize_tonlib_network_config(&c.network_config),
            network_name: c.network_name,
            verbosity: c.verbosity,
            keystore: c.keystore.into(),
            last_block_threshold: Duration::from_secs(c.last_block_threshold_sec),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NetworkConfig {
    lite_servers: Vec<NetworkConfigLiteServer>,
    zero_state: NetworkConfigZeroState,
}

fn serialize_tonlib_network_config(config: &NetworkConfig) -> String {
    #[derive(Serialize)]
    struct TonlibConfig<'a> {
        #[serde(
            serialize_with = "serialize_lite_servers",
            rename(serialize = "liteservers")
        )]
        lite_servers: &'a [NetworkConfigLiteServer],
        #[serde(
            serialize_with = "serialize_zero_state",
            rename(serialize = "validator")
        )]
        zero_state: &'a NetworkConfigZeroState,
    }

    serde_json::to_string(&TonlibConfig {
        lite_servers: &config.lite_servers,
        zero_state: &config.zero_state,
    })
    .expect("failed to serialize tonlib network config")
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NetworkConfigLiteServer {
    addr: SocketAddrV4,
    public_key: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NetworkConfigZeroState {
    root_hash: String,
    file_hash: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum KeystoreType {
    InMemory,
    FileSystem { root_dir: String },
}

impl From<KeystoreType> for tonlib::KeystoreType {
    fn from(t: KeystoreType) -> Self {
        match t {
            KeystoreType::InMemory => Self::InMemory,
            KeystoreType::FileSystem { root_dir } => Self::FileSystem(root_dir),
        }
    }
}

fn serialize_lite_servers<S>(
    lite_servers: &[NetworkConfigLiteServer],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    #[derive(Serialize)]
    pub struct Helper<'a> {
        #[serde(serialize_with = "serialize_ip_addr")]
        ip: &'a Ipv4Addr,
        port: u16,
        #[serde(serialize_with = "serialize_public_key", rename(serialize = "id"))]
        public_key: &'a str,
    }

    let mut arr = serializer.serialize_seq(Some(lite_servers.len()))?;
    for item in lite_servers.iter() {
        arr.serialize_element(&Helper {
            ip: &item.addr.ip(),
            port: item.addr.port(),
            public_key: &item.public_key,
        })?;
    }
    arr.end()
}

fn serialize_ip_addr<S>(ip: &Ipv4Addr, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let octets = ip.octets();
    let mut result = 0;
    for (i, &octet) in octets.iter().enumerate() {
        result += (octet as u32) << (24 - i * 8);
    }
    serializer.serialize_i32(result as i32)
}

fn serialize_public_key<S>(key: &str, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    #[derive(Serialize)]
    struct Helper<'a> {
        #[serde(rename = "@type")]
        ty: &'a str,
        key: &'a str,
    }
    Helper {
        ty: "pub.ed25519",
        key,
    }
    .serialize(serializer)
}

fn serialize_zero_state<S>(
    zero_state: &NetworkConfigZeroState,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    #[derive(Serialize)]
    struct ZeroState<'a> {
        workchain: i8,
        shard: i64,
        seqno: i32,
        root_hash: &'a str,
        file_hash: &'a str,
    }

    #[derive(Serialize)]
    struct Helper<'a> {
        #[serde(rename = "@type")]
        ty: &'a str,
        zero_state: ZeroState<'a>,
    }

    Helper {
        ty: "validator.config.global",
        zero_state: ZeroState {
            workchain: MASTERCHAIN_ID,
            shard: MASTERCHAIN_SHARD as i64,
            seqno: 0,
            root_hash: &zero_state.root_hash,
            file_hash: &zero_state.file_hash,
        },
    }
    .serialize(serializer)
}

pub fn default_mainnet_config() -> NetworkConfig {
    NetworkConfig {
        lite_servers: vec![NetworkConfigLiteServer {
            addr: SocketAddrV4::new(Ipv4Addr::new(54, 158, 97, 195), 3031),
            public_key: "uNRRL+6enQjuiZ/s6Z+vO7yxUUR7uxdfzIy+RxkECrc=".to_owned(),
        }],
        zero_state: NetworkConfigZeroState {
            root_hash: "WP/KGheNr/cF3lQhblQzyb0ufYUAcNM004mXhHq56EU=".to_owned(),
            file_hash: "0nC4eylStbp9qnCq8KjDYb789NjS25L5ZA1UQwcIOOQ=".to_owned(),
        },
    }
}

pub fn default_testnet_config() -> NetworkConfig {
    NetworkConfig {
        lite_servers: vec![NetworkConfigLiteServer {
            addr: SocketAddrV4::new(Ipv4Addr::new(54, 158, 97, 195), 3032),
            public_key: "uNRRL+6enQjuiZ/s6Z+vO7yxUUR7uxdfzIy+RxkECrc=".to_owned(),
        }],
        zero_state: NetworkConfigZeroState {
            root_hash: "hw7m9dVxoI9QzNp8jlOA9fxZeojPQS+T12KTEAXFzS8=".to_owned(),
            file_hash: "OqQ7U32OCh0RI9EeRs9rCR78C6075Ff9WFk653M2qBk=".to_owned(),
        },
    }
}

pub const MASTERCHAIN_ID: i8 = -1;
pub const MASTERCHAIN_SHARD: u64 = 0x8000000000000000;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const TARGET_CONFIG: &str = r#"{
      "liteservers": [
        {
          "ip": 916349379,
          "port": 3031,
          "id": {
            "@type": "pub.ed25519",
            "key": "uNRRL+6enQjuiZ/s6Z+vO7yxUUR7uxdfzIy+RxkECrc="
          }
        }
      ],
      "validator": {
        "@type": "validator.config.global",
        "zero_state": {
          "workchain": -1,
          "shard": -9223372036854775808,
          "seqno": 0,
          "root_hash": "WP/KGheNr/cF3lQhblQzyb0ufYUAcNM004mXhHq56EU=",
          "file_hash": "0nC4eylStbp9qnCq8KjDYb789NjS25L5ZA1UQwcIOOQ="
        }
      }
    }"#;

    #[test]
    fn test_serialization() {
        let target_json = serde_json::from_str::<Value>(TARGET_CONFIG).unwrap();

        let config = r#"{
            "lite_servers": [
                {                
                    "addr": "54.158.97.195:3031",
                    "public_key": "uNRRL+6enQjuiZ/s6Z+vO7yxUUR7uxdfzIy+RxkECrc="
                }
            ],
            "zero_state": {
                "file_hash": "0nC4eylStbp9qnCq8KjDYb789NjS25L5ZA1UQwcIOOQ=",
                "root_hash": "WP/KGheNr/cF3lQhblQzyb0ufYUAcNM004mXhHq56EU=",
                "shard": -9223372036854775808,
                "seqno": 0,
                "workchain": -1
            }
        }"#;
        let custom_json = serde_json::from_str::<NetworkConfig>(config).unwrap();

        let serialized_config = serialize_tonlib_network_config(&custom_json);
        let deserialized_config = serde_json::from_str::<Value>(&serialized_config).unwrap();

        assert_eq!(
            serde_json::to_string(&deserialized_config).unwrap(),
            serde_json::to_string(&target_json).unwrap()
        );
    }
}