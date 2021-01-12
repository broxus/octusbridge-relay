use crate::config::RelayConfig;
use crate::crypto::key_managment::KeyData;
use crate::engine::bridge::*;
use crate::prelude::*;
use num_bigint::BigUint;
pub use relay_models::models::{InitData, Password, RescanEthData, Status, VotingAddress};
use relay_ton::contracts;

impl State {
    pub async fn finalize(&mut self, config: RelayConfig, key_data: KeyData) -> Result<(), Error> {
        let bridge = make_bridge(self.state_manager.clone(), config, key_data).await?;

        log::info!("Successfully initialized");

        self.bridge_state = BridgeState::Running(bridge);
        Ok(())
    }
}

pub struct State {
    pub state_manager: Db,
    pub bridge_state: BridgeState,
}

pub enum BridgeState {
    Uninitialized,
    Locked,
    Running(Arc<Bridge>),
}

#[derive(Deserialize, Serialize)]
pub struct EventConfiguration {
    pub configuration_id: String,
    pub ethereum_event_abi: String,
    pub ethereum_event_address: String,
    pub event_proxy_address: String,
    pub ethereum_event_blocks_to_confirm: u64,
    pub event_required_confirmations: u64,
    pub event_required_rejects: u64,
    pub event_initial_balance: u64,
    pub bridge_address: String,
    pub event_code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, opg::OpgModel)]
#[serde(rename_all = "lowercase", tag = "vote", content = "address")]
pub enum Voting {
    Confirm(String),
    Reject(String),
}

impl TryFrom<Voting> for (BigUint, contracts::models::Voting) {
    type Error = anyhow::Error;

    fn try_from(value: Voting) -> Result<Self, Self::Error> {
        let (address, voting) = match value {
            Voting::Confirm(address) => (address, contracts::models::Voting::Confirm),
            Voting::Reject(address) => (address, contracts::models::Voting::Reject),
        };
        let configuration_id =
            BigUint::from_str(&address).map_err(|e| anyhow!("{}", e.to_string()))?;
        Ok((configuration_id, voting))
    }
}

impl From<(BigUint, contracts::models::EthEventConfiguration)> for EventConfiguration {
    fn from((configuration_id, c): (BigUint, contracts::models::EthEventConfiguration)) -> Self {
        Self {
            configuration_id: configuration_id.to_string(),
            ethereum_event_abi: c.common.event_abi,
            ethereum_event_address: hex::encode(c.event_address.as_bytes()),
            event_proxy_address: c.proxy_address.to_string(),
            ethereum_event_blocks_to_confirm: c
                .event_blocks_to_confirm
                .to_u64()
                .unwrap_or(u64::max_value()),
            event_required_confirmations: c
                .common
                .event_required_confirmations
                .to_u64()
                .unwrap_or(u64::max_value()),
            event_required_rejects: c
                .common
                .event_required_rejects
                .to_u64()
                .unwrap_or(u64::max_value()),
            event_initial_balance: c
                .common
                .event_initial_balance
                .to_u64()
                .unwrap_or(u64::max_value()),
            bridge_address: c.common.bridge_address.to_string(),
            event_code: relay_ton::prelude::serialize_toc(&c.common.event_code)
                .map(hex::encode)
                .unwrap_or_default(),
        }
    }
}
