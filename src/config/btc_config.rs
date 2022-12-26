use serde::{Deserialize, Serialize};

use crate::utils::*;

/// Solana network settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BtcConfig {
    /// RPC endpoint
    pub esplora_url: String,

    /// Change address for all EVER -> BTC transactions
    pub change_address: String,

    /// Poll masterchain blocks count in Ever. Default: 1000
    #[serde(default = "const_u64::<1000>")]
    pub poll_mc_blocks_count: u64,

    /// Connection timeout. Default: 60
    #[serde(default = "const_u64::<60>")]
    pub connection_timeout_sec: u64,

    /// Max simultaneous connection count. Default: 10
    #[serde(default = "const_usize::<10>")]
    pub pool_size: usize,

    /// Blocks polling interval. Default: 15
    #[serde(default = "const_u64::<30>")]
    pub poll_interval_sec: u64,

    /// Max request duration (including all failed retires). Default: 604800
    #[serde(default = "const_u64::<604800>")]
    pub maximum_failed_responses_time_sec: u64,
}
