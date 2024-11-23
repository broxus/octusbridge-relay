use std::path::PathBuf;

use nekoton_utils::*;
use secstr::SecUtf8;
use serde::{Deserialize, Serialize};

pub use self::eth_config::*;
pub use self::sol_config::*;
pub use self::stored_keys::*;
pub use self::verification_state::*;

mod eth_config;
mod sol_config;
mod stored_keys;
mod verification_state;

/// Main application config (full). Used to run relay
#[derive(Serialize, Deserialize)]
pub struct AppConfig {
    /// Password, used to encode and decode data in keystore
    pub master_password: SecUtf8,

    /// Staker address from which keys were submitted
    #[serde(with = "serde_address")]
    pub staker_address: ton_block::MsgAddressInt,

    /// Bridge related settings
    pub bridge_settings: BridgeConfig,

    /// Prometheus metrics exporter settings.
    /// Completely disable when not specified
    #[serde(default)]
    pub metrics_settings: Option<pomfrit::Config>,
}

/// Main application config (brief). Used for simple commands that require only password
#[derive(Serialize, Deserialize)]
pub struct BriefAppConfig {
    /// Password, used to encode and decode data in keystore
    #[serde(default)]
    pub master_password: Option<SecUtf8>,
}

/// Bridge related settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BridgeConfig {
    /// Path to the file with keystore data
    pub keys_path: PathBuf,

    /// Bridge contract address
    #[serde(with = "serde_address")]
    pub bridge_address: ton_block::MsgAddressInt,

    /// If set, relay will not participate in elections. Default: false
    #[serde(default)]
    pub ignore_elections: bool,

    /// EVM networks settings
    pub evm_networks: Vec<EthConfig>,

    /// Solana network settings
    #[serde(default)]
    pub sol_network: Option<SolConfig>,

    /// ETH address verification settings
    #[serde(default)]
    pub address_verification: AddressVerificationConfig,

    /// Ton token metadata endpoint base url
    #[cfg(feature = "ton")]
    pub token_meta_base_url: String,

    pub rpc_endpoints: Vec<url::Url>,
}

/// ETH address verification settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AddressVerificationConfig {
    /// Minimal balance on user's wallet to start address verification
    /// Default: 50000000 (0.05 ETH)
    pub min_balance_gwei: u64,

    /// Fixed gas price. Default: 300
    pub gas_price_gwei: u64,

    /// Path to the file with transaction state.
    /// Default: `./verification-state.json`
    pub state_path: PathBuf,
}

impl Default for AddressVerificationConfig {
    fn default() -> Self {
        Self {
            min_balance_gwei: 50000000,
            gas_price_gwei: 300,
            state_path: "verification-state.json".into(),
        }
    }
}
