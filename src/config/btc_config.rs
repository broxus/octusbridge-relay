use serde::{Deserialize, Serialize};

use crate::utils::*;

/// BTC network settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BtcConfig {
    /// RPC endpoint
    pub esplora_url: String,

    /// Backup public keys
    pub bkp_public_keys: [String; 3],

    /// BTC network
    pub network: bitcoin::Network,

    /// Max simultaneous connection count. Default: 10
    #[serde(default = "const_usize::<10>")]
    pub pool_size: usize,

    /// Blocks polling interval. Default: 60
    #[serde(default = "const_u64::<60>")]
    pub poll_interval_sec: u64,

    /// Max request duration (including all failed retires). Default: 604800
    #[serde(default = "const_u64::<604800>")]
    pub maximum_failed_responses_time_sec: u64,

    /// Allowed DB size in bytes. Default: one third of all machine RAM
    #[serde(default = "const_usize::<0>")]
    pub max_db_memory_usage: usize,
}
