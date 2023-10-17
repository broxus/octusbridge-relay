use serde::{Deserialize, Serialize};
use solana_sdk::commitment_config::CommitmentConfig;

use crate::utils::*;

/// Solana network settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolConfig {
    /// RPC endpoints
    pub endpoints: Vec<String>,

    /// Connection timeout. Default: 60
    #[serde(default = "const_u64::<60>")]
    pub connection_timeout_sec: u64,

    /// Commitment level
    #[serde(with = "serde_commitment")]
    pub commitment: CommitmentConfig,

    /// Max simultaneous connection count. Default: 10
    #[serde(default = "const_usize::<10>")]
    pub pool_size: usize,

    /// Blocks polling interval. Default: 15
    #[serde(default = "const_u64::<30>")]
    pub poll_interval_sec: u64,

    /// Max request duration (including all failed retires). Default: 604800
    #[serde(default = "const_u64::<604800>")]
    pub maximum_failed_responses_time_sec: u64,

    /// Proposals polling interval. Default: 3600
    #[serde(default = "const_u64::<3600>")]
    pub poll_proposals_interval_sec: u64,

    /// Deposits polling timeout. Default: 120
    #[serde(default = "const_u64::<120>")]
    pub poll_deposits_timeout_sec: u64,

    /// Deposits polling interval. Default: 5
    #[serde(default = "const_u64::<5>")]
    pub poll_deposits_interval_sec: u64,

    /// Skip invalid TON->SOL event is Solana. Default: false
    #[serde(default)]
    pub clear_invalid_events: bool,
}
