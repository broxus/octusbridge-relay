use std::collections::hash_map;
use std::future::Future;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use borsh::BorshDeserialize;
use eth_ton_abi_converter::{
    decode_ton_event_abi, make_mapped_ton_event, map_ton_tokens_to_eth_bytes, EthEventAbi,
    EthToTonMappingContext,
};
use nekoton_abi::*;
use nekoton_utils::TrustMe;
use rustc_hash::{FxHashMap, FxHashSet};
use solana_bridge::bridge_errors::SolanaBridgeError;
use solana_client::client_error::{ClientError, ClientErrorKind};
use solana_client::rpc_request::{RpcError, RpcResponseErrorData};
use solana_client::rpc_response::RpcSimulateTransactionResult;
use solana_sdk::instruction::InstructionError;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::transaction::TransactionError;
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use ton_abi::TokenValue;
use ton_block::{Deserializable, HashmapAugType};
use ton_types::UInt256;

use crate::engine::keystore::*;
use crate::engine::sol_subscriber::*;
use crate::engine::ton_contracts::*;
use crate::engine::ton_subscriber::*;
use crate::engine::EngineContext;
use crate::utils::*;

/// Events part of relays logic
pub struct Bridge {
    /// Shared engine context
    context: Arc<EngineContext>,

    /// Bridge contract address
    bridge_account: UInt256,
    /// Bridge events listener
    bridge_observer: Arc<AccountObserver<BridgeEvent>>,
    /// Known contracts
    state: RwLock<BridgeState>,

    // Observers for pending ETH->TON events
    eth_ton_events_state: Arc<EventsState<EthTonEvent>>,

    // Observers for pending TON->ETH events
    ton_eth_events_state: Arc<EventsState<TonEthEvent>>,

    // Observers for pending SOL->TON events
    sol_ton_events_state: Arc<EventsState<SolTonEvent>>,

    // Observers for pending TON->SOL events
    ton_sol_events_state: Arc<EventsState<TonSolEvent>>,

    // Observers for pending BTC->TON events
    btc_ton_events_state: Arc<EventsState<BtcTonEvent>>,

    // Observers for pending TON->BTC events
    ton_btc_events_state: Arc<EventsState<TonBtcEvent>>,

    connectors_tx: AccountEventsTx<ConnectorEvent>,
    eth_ton_event_configurations_tx: AccountEventsTx<EthTonEventConfigurationEvent>,
    ton_eth_event_configurations_tx: AccountEventsTx<TonEthEventConfigurationEvent>,
    sol_ton_event_configurations_tx: AccountEventsTx<SolTonEventConfigurationEvent>,
    ton_sol_event_configurations_tx: AccountEventsTx<TonSolEventConfigurationEvent>,
    btc_ton_event_configurations_tx: AccountEventsTx<BtcTonEventConfigurationEvent>,
    ton_btc_event_configurations_tx: AccountEventsTx<TonBtcEventConfigurationEvent>,

    total_active_eth_ton_event_configurations: AtomicUsize,
    total_active_ton_eth_event_configurations: AtomicUsize,
    total_active_sol_ton_event_configurations: AtomicUsize,
    total_active_ton_sol_event_configurations: AtomicUsize,
    total_active_btc_ton_event_configurations: AtomicUsize,
    total_active_ton_btc_event_configurations: AtomicUsize,
}

impl Bridge {
    pub async fn new(context: Arc<EngineContext>, bridge_account: UInt256) -> Result<Arc<Self>> {
        // Create bridge
        let (bridge_events_tx, bridge_events_rx) = mpsc::unbounded_channel();
        let (connectors_tx, connectors_rx) = mpsc::unbounded_channel();
        let (eth_ton_event_configurations_tx, eth_ton_event_configurations_rx) =
            mpsc::unbounded_channel();
        let (ton_eth_event_configurations_tx, ton_eth_event_configurations_rx) =
            mpsc::unbounded_channel();
        let (sol_ton_event_configurations_tx, sol_ton_event_configurations_rx) =
            mpsc::unbounded_channel();
        let (ton_sol_event_configurations_tx, ton_sol_event_configurations_rx) =
            mpsc::unbounded_channel();
        let (btc_ton_event_configurations_tx, btc_ton_event_configurations_rx) =
            mpsc::unbounded_channel();
        let (ton_btc_event_configurations_tx, ton_btc_event_configurations_rx) =
            mpsc::unbounded_channel();
        let (eth_ton_events_tx, eth_ton_events_rx) = mpsc::unbounded_channel();
        let (ton_eth_events_tx, ton_eth_events_rx) = mpsc::unbounded_channel();
        let (sol_ton_events_tx, sol_ton_events_rx) = mpsc::unbounded_channel();
        let (ton_sol_events_tx, ton_sol_events_rx) = mpsc::unbounded_channel();
        let (btc_ton_events_tx, btc_ton_events_rx) = mpsc::unbounded_channel();
        let (ton_btc_events_tx, ton_btc_events_rx) = mpsc::unbounded_channel();

        let bridge_observer = AccountObserver::new(&bridge_events_tx);

        let bridge = Arc::new(Bridge {
            context,
            bridge_account,
            bridge_observer: bridge_observer.clone(),
            state: Default::default(),
            eth_ton_events_state: EventsState::new(eth_ton_events_tx),
            ton_eth_events_state: EventsState::new(ton_eth_events_tx),
            sol_ton_events_state: EventsState::new(sol_ton_events_tx),
            ton_sol_events_state: EventsState::new(ton_sol_events_tx),
            btc_ton_events_state: EventsState::new(btc_ton_events_tx),
            ton_btc_events_state: EventsState::new(ton_btc_events_tx),
            connectors_tx,
            eth_ton_event_configurations_tx,
            ton_eth_event_configurations_tx,
            sol_ton_event_configurations_tx,
            ton_sol_event_configurations_tx,
            btc_ton_event_configurations_tx,
            ton_btc_event_configurations_tx,
            total_active_eth_ton_event_configurations: Default::default(),
            total_active_ton_eth_event_configurations: Default::default(),
            total_active_sol_ton_event_configurations: Default::default(),
            total_active_ton_sol_event_configurations: Default::default(),
            total_active_btc_ton_event_configurations: Default::default(),
            total_active_ton_btc_event_configurations: Default::default(),
        });

        // Prepare listeners
        start_listening_events(
            &bridge,
            "BridgeContract",
            bridge_events_rx,
            Self::process_bridge_event,
        );

        start_listening_events(
            &bridge,
            "ConnectorContract",
            connectors_rx,
            Self::process_connector_event,
        );

        start_listening_events(
            &bridge,
            "EthEventConfigurationContract",
            eth_ton_event_configurations_rx,
            Self::process_eth_ton_event_configuration_event,
        );

        start_listening_events(
            &bridge,
            "TonEthEventConfigurationContract",
            ton_eth_event_configurations_rx,
            Self::process_ton_eth_event_configuration_event,
        );

        if bridge.context.sol_subscriber.is_some() {
            start_listening_events(
                &bridge,
                "SolTonEventConfigurationContract",
                sol_ton_event_configurations_rx,
                Self::process_sol_ton_event_configuration_event,
            );

            start_listening_events(
                &bridge,
                "TonSolEventConfigurationContract",
                ton_sol_event_configurations_rx,
                Self::process_ton_sol_event_configuration_event,
            );
        }

        if bridge.context.btc_subscriber.is_some() {
            start_listening_events(
                &bridge,
                "BtcTonEventConfigurationContract",
                btc_ton_event_configurations_rx,
                Self::process_btc_ton_event_configuration_event,
            );

            start_listening_events(
                &bridge,
                "TonBtcEventConfigurationContract",
                ton_btc_event_configurations_rx,
                Self::process_ton_btc_event_configuration_event,
            );
        }

        start_listening_events(
            &bridge,
            "EthTonEventContract",
            eth_ton_events_rx,
            Self::process_eth_ton_event,
        );

        start_listening_events(
            &bridge,
            "TonEthEventContract",
            ton_eth_events_rx,
            Self::process_ton_eth_event,
        );

        if bridge.context.sol_subscriber.is_some() {
            start_listening_events(
                &bridge,
                "SolTonEventContract",
                sol_ton_events_rx,
                Self::process_sol_ton_event,
            );

            start_listening_events(
                &bridge,
                "TonSolEventContract",
                ton_sol_events_rx,
                Self::process_ton_sol_event,
            );
        }

        if bridge.context.btc_subscriber.is_some() {
            start_listening_events(
                &bridge,
                "BtcTonEventContract",
                btc_ton_events_rx,
                Self::process_btc_ton_event,
            );

            start_listening_events(
                &bridge,
                "TonBtcEventContract",
                ton_btc_events_rx,
                Self::process_ton_btc_event,
            );
        }

        // Subscribe bridge account to transactions
        bridge
            .context
            .ton_subscriber
            .add_transactions_subscription([bridge.bridge_account], &bridge.bridge_observer);

        // Initialize
        bridge.get_all_configurations().await?;
        bridge.get_all_events().await?;

        bridge.start_event_configurations_gc();

        Ok(bridge)
    }

    pub fn metrics(&self) -> BridgeMetrics {
        BridgeMetrics {
            pending_eth_ton_event_count: self.eth_ton_events_state.count.load(Ordering::Acquire),
            pending_ton_eth_event_count: self.ton_eth_events_state.count.load(Ordering::Acquire),
            pending_sol_ton_event_count: self.sol_ton_events_state.count.load(Ordering::Acquire),
            pending_ton_sol_event_count: self.ton_sol_events_state.count.load(Ordering::Acquire),
            pending_btc_ton_event_count: self.btc_ton_events_state.count.load(Ordering::Acquire),
            pending_ton_btc_event_count: self.ton_btc_events_state.count.load(Ordering::Acquire),
            total_active_eth_ton_event_configurations: self
                .total_active_eth_ton_event_configurations
                .load(Ordering::Acquire),
            total_active_ton_eth_event_configurations: self
                .total_active_ton_eth_event_configurations
                .load(Ordering::Acquire),
            total_active_sol_ton_event_configurations: self
                .total_active_sol_ton_event_configurations
                .load(Ordering::Acquire),
            total_active_ton_sol_event_configurations: self
                .total_active_ton_sol_event_configurations
                .load(Ordering::Acquire),
            total_active_btc_ton_event_configurations: self
                .total_active_btc_ton_event_configurations
                .load(Ordering::Acquire),
            total_active_ton_btc_event_configurations: self
                .total_active_ton_btc_event_configurations
                .load(Ordering::Acquire),
        }
    }

    async fn process_bridge_event(
        self: Arc<Self>,
        (_, event): (UInt256, BridgeEvent),
    ) -> Result<()> {
        match event {
            BridgeEvent::ConnectorDeployed(event) => {
                // Create connector entry if it wasn't already created
                match self.state.write().await.connectors.entry(event.connector) {
                    hash_map::Entry::Vacant(entry) => {
                        // Create observer
                        let observer = AccountObserver::new(&self.connectors_tx);

                        let entry = entry.insert(observer);

                        // Subscribe observer to transactions
                        self.context
                            .ton_subscriber
                            .add_transactions_subscription([event.connector], entry);
                    }
                    hash_map::Entry::Occupied(_) => {
                        tracing::error!(
                            connector = %DisplayAddr(event.connector),
                            "got connector deployment event but it already exists",
                        );
                        return Ok(());
                    }
                };

                // Check connector contract if it was added in this iteration
                tokio::spawn(async move {
                    if let Err(e) = self.check_connector_contract(event.connector).await {
                        tracing::error!("failed to check connector contract: {e:?}");
                    }
                });
            }
        }

        Ok(())
    }

    async fn process_connector_event(
        self: Arc<Self>,
        (connector, event): (UInt256, ConnectorEvent),
    ) -> Result<()> {
        match event {
            ConnectorEvent::Enable => self.check_connector_contract(connector).await,
        }
    }

    async fn process_eth_ton_event_configuration_event(
        self: Arc<Self>,
        (account, event): (UInt256, EthTonEventConfigurationEvent),
    ) -> Result<()> {
        match event {
            // Create observer on each deployment event
            EthTonEventConfigurationEvent::EventsDeployed { events } => {
                for address in events {
                    if self.add_pending_event(address, &self.eth_ton_events_state) {
                        let this = self.clone();
                        self.spawn_background_task("preprocess ETH->TON event", async move {
                            this.preprocess_event(address, &this.eth_ton_events_state)
                                .await
                        });
                    }
                }
            }
            // Update configuration state
            EthTonEventConfigurationEvent::SetEndBlockNumber { end_block_number } => {
                let mut state = self.state.write().await;
                let configuration = state
                    .eth_ton_event_configurations
                    .get_mut(&account)
                    .ok_or(BridgeError::UnknownConfiguration)?;
                configuration.details.network_configuration.end_block_number = end_block_number;
            }
        }
        Ok(())
    }

    async fn process_ton_eth_event_configuration_event(
        self: Arc<Self>,
        (account, event): (UInt256, TonEthEventConfigurationEvent),
    ) -> Result<()> {
        match event {
            // Create observer on each deployment event
            TonEthEventConfigurationEvent::EventDeployed { address, .. } => {
                if self.add_pending_event(address, &self.ton_eth_events_state) {
                    let this = self.clone();
                    self.spawn_background_task("preprocess TON->ETH event", async move {
                        this.preprocess_event(address, &this.ton_eth_events_state)
                            .await
                    });
                } else {
                    // NOTE: Each TON event must be unique on the contracts level,
                    // so receiving message with duplicated address is
                    // a signal that something went wrong
                    tracing::warn!(
                        configuration = %DisplayAddr(account),
                        event = %DisplayAddr(address),
                        "got deployment message for pending event",
                    );
                }
            }
            // Update configuration state
            TonEthEventConfigurationEvent::SetEndTimestamp { end_timestamp } => {
                let mut state = self.state.write().await;
                let configuration = state
                    .ton_eth_event_configurations
                    .get_mut(&account)
                    .ok_or(BridgeError::UnknownConfiguration)?;
                configuration.details.network_configuration.end_timestamp = end_timestamp;
            }
        }
        Ok(())
    }

    async fn process_sol_ton_event_configuration_event(
        self: Arc<Self>,
        (account, event): (UInt256, SolTonEventConfigurationEvent),
    ) -> Result<()> {
        match event {
            // Create observer on each deployment event
            SolTonEventConfigurationEvent::EventDeployed { address } => {
                if self.add_pending_event(address, &self.sol_ton_events_state) {
                    let this = self.clone();
                    self.spawn_background_task("preprocess SOL->TON event", async move {
                        this.preprocess_event(address, &this.sol_ton_events_state)
                            .await
                    });
                }
            }
            // Update configuration state
            SolTonEventConfigurationEvent::SetEndTimestamp { end_timestamp } => {
                let mut state = self.state.write().await;
                let configuration = state
                    .sol_ton_event_configurations
                    .get_mut(&account)
                    .ok_or(BridgeError::UnknownConfiguration)?;
                configuration.details.network_configuration.end_timestamp = end_timestamp;
            }
        }
        Ok(())
    }

    async fn process_ton_sol_event_configuration_event(
        self: Arc<Self>,
        (account, event): (UInt256, TonSolEventConfigurationEvent),
    ) -> Result<()> {
        match event {
            // Create observer on each deployment event
            TonSolEventConfigurationEvent::EventDeployed { address, .. } => {
                if self.add_pending_event(address, &self.ton_sol_events_state) {
                    let this = self.clone();
                    self.spawn_background_task("preprocess TON->SOL event", async move {
                        this.preprocess_event(address, &this.ton_sol_events_state)
                            .await
                    });
                } else {
                    // NOTE: Each TON event must be unique on the contracts level,
                    // so receiving message with duplicated address is
                    // a signal that something went wrong
                    tracing::warn!(
                        configuration = %DisplayAddr(account),
                        event = %DisplayAddr(address),
                        "got deployment message for pending event",
                    );
                }
            }
            // Update configuration state
            TonSolEventConfigurationEvent::SetEndTimestamp { end_timestamp } => {
                let mut state = self.state.write().await;
                let configuration = state
                    .ton_sol_event_configurations
                    .get_mut(&account)
                    .ok_or(BridgeError::UnknownConfiguration)?;
                configuration.details.network_configuration.end_timestamp = end_timestamp;
            }
        }
        Ok(())
    }

    async fn process_btc_ton_event_configuration_event(
        self: Arc<Self>,
        (account, event): (UInt256, BtcTonEventConfigurationEvent),
    ) -> Result<()> {
        match event {
            // Create observer on each deployment event
            BtcTonEventConfigurationEvent::EventsDeployed { events } => {
                for address in events {
                    if self.add_pending_event(address, &self.btc_ton_events_state) {
                        let this = self.clone();
                        self.spawn_background_task("preprocess BTC->TON event", async move {
                            this.preprocess_event(address, &this.btc_ton_events_state)
                                .await
                        });
                    }
                }
            }
            // Update configuration state
            BtcTonEventConfigurationEvent::SetEndBlockNumber { end_block_number } => {
                let mut state = self.state.write().await;
                let configuration = state
                    .btc_ton_event_configurations
                    .get_mut(&account)
                    .ok_or(BridgeError::UnknownConfiguration)?;
                configuration.details.network_configuration.end_block_number = end_block_number;
            }
        }
        Ok(())
    }

    async fn process_ton_btc_event_configuration_event(
        self: Arc<Self>,
        (account, event): (UInt256, TonBtcEventConfigurationEvent),
    ) -> Result<()> {
        match event {
            // Create observer on each deployment event
            TonBtcEventConfigurationEvent::EventDeployed { address, .. } => {
                if self.add_pending_event(address, &self.ton_btc_events_state) {
                    let this = self.clone();
                    self.spawn_background_task("preprocess TON->BTC event", async move {
                        this.preprocess_event(address, &this.ton_btc_events_state)
                            .await
                    });
                } else {
                    // NOTE: Each TON event must be unique on the contracts level,
                    // so receiving message with duplicated address is
                    // a signal that something went wrong
                    tracing::warn!(
                        configuration = %DisplayAddr(account),
                        event = %DisplayAddr(address),
                        "got deployment message for pending event",
                    );
                }
            }
            // Update configuration state
            TonBtcEventConfigurationEvent::SetEndTimestamp { end_timestamp } => {
                let mut state = self.state.write().await;
                let configuration = state
                    .ton_btc_event_configurations
                    .get_mut(&account)
                    .ok_or(BridgeError::UnknownConfiguration)?;
                configuration.details.network_configuration.end_timestamp = end_timestamp;
            }
        }
        Ok(())
    }

    async fn process_eth_ton_event(
        self: Arc<Self>,
        (account, event): (UInt256, (EthTonEvent, EventStatus)),
    ) -> Result<()> {
        use dashmap::mapref::entry::Entry;

        let our_public_key = self.context.keystore.ton.public_key();

        // Use flag to update counter outside events map lock to reduce its duration
        let mut event_removed = false;

        // Handle only known ETH events
        if let Entry::Occupied(entry) = self.eth_ton_events_state.pending.entry(account) {
            let remove_entry = || {
                // Remove pending event
                entry.remove();
                event_removed = true;
            };

            match event {
                // Remove event if voting process was finished
                (EthTonEvent::Rejected, _)
                | (_, EventStatus::Confirmed | EventStatus::Rejected) => remove_entry(),
                // Handle event initialization
                (EthTonEvent::ReceiveRoundRelays { keys }, _) => {
                    // Check if event contains our key
                    if keys.contains(our_public_key) {
                        // Start voting
                        self.spawn_background_task(
                            "update ETH->TON event",
                            self.clone().update_eth_ton_event(account),
                        );
                    } else {
                        remove_entry();
                    }
                }
                // Handle our confirmation or rejection
                (EthTonEvent::Confirm { public_key } | EthTonEvent::Reject { public_key }, _)
                    if public_key == our_public_key =>
                {
                    remove_entry()
                }
                _ => { /* Ignore other events */ }
            }
        }

        // Update metrics
        if event_removed {
            self.eth_ton_events_state
                .count
                .fetch_sub(1, Ordering::Release);
        }

        Ok(())
    }

    async fn process_ton_eth_event(
        self: Arc<Self>,
        (account, event): (UInt256, (TonEthEvent, EventStatus)),
    ) -> Result<()> {
        use dashmap::mapref::entry::Entry;

        let our_public_key = self.context.keystore.ton.public_key();

        // Use flag to update counter outside events map lock to reduce its duration
        let mut event_removed = false;

        // Handle only known TON events
        if let Entry::Occupied(entry) = self.ton_eth_events_state.pending.entry(account) {
            let remove_entry = || {
                // Remove pending event
                entry.remove();
                event_removed = true;
            };

            match event {
                // Remove event in confirmed state if the balance is not enough.
                //
                // NOTE: it is not strictly necessary to collect all signatures, so the
                // contract subscription is allowed to be dropped on nearly empty balance.
                //
                // This state can be achieved by calling `close` method on transfer contract
                // or execution `confirm` or `reject` after several years so that the cost of
                // keeping the contract almost nullifies its balance.
                (TonEthEvent::Closed, EventStatus::Confirmed) => remove_entry(),
                // Remove event if it was rejected
                (TonEthEvent::Rejected, _) | (_, EventStatus::Rejected) => remove_entry(),
                // Handle event initialization
                (TonEthEvent::ReceiveRoundRelays { keys }, _) => {
                    // Check if event contains our key
                    if keys.contains(our_public_key) {
                        // Start voting
                        self.spawn_background_task(
                            "update TON->ETH event",
                            self.clone().update_ton_eth_event(account),
                        );
                    } else {
                        remove_entry();
                    }
                }
                // Handle our confirmation or rejection
                (TonEthEvent::Confirm { public_key } | TonEthEvent::Reject { public_key }, _)
                    if public_key == our_public_key =>
                {
                    remove_entry();
                }
                _ => { /* Ignore other events */ }
            }
        }

        // Update metrics
        if event_removed {
            self.ton_eth_events_state
                .count
                .fetch_sub(1, Ordering::Release);
        }

        Ok(())
    }

    async fn process_sol_ton_event(
        self: Arc<Self>,
        (account, event): (UInt256, (SolTonEvent, EventStatus)),
    ) -> Result<()> {
        use dashmap::mapref::entry::Entry;

        let our_public_key = self.context.keystore.sol.public_key_bytes();

        // Use flag to update counter outside events map lock to reduce its duration
        let mut event_removed = false;

        // Handle only known SOL events
        if let Entry::Occupied(entry) = self.sol_ton_events_state.pending.entry(account) {
            let remove_entry = || {
                // Remove pending event
                entry.remove();
                event_removed = true;
            };

            match event {
                // Remove event if voting process was finished
                (SolTonEvent::Rejected, _)
                | (_, EventStatus::Confirmed | EventStatus::Rejected) => remove_entry(),
                // Handle event initialization
                (SolTonEvent::ReceiveRoundRelays { keys }, _) => {
                    // Check if event contains our key
                    if keys.contains(our_public_key) {
                        // Start voting
                        self.spawn_background_task(
                            "update SOL->TON event",
                            self.clone().update_sol_ton_event(account),
                        );
                    } else {
                        remove_entry();
                    }
                }
                // Handle our confirmation or rejection
                (SolTonEvent::Confirm { public_key } | SolTonEvent::Reject { public_key }, _)
                    if public_key == our_public_key =>
                {
                    remove_entry()
                }
                _ => { /* Ignore other events */ }
            }
        }

        // Update metrics
        if event_removed {
            self.sol_ton_events_state
                .count
                .fetch_sub(1, Ordering::Release);
        }

        Ok(())
    }

    async fn process_ton_sol_event(
        self: Arc<Self>,
        (account, event): (UInt256, (TonSolEvent, EventStatus)),
    ) -> Result<()> {
        use dashmap::mapref::entry::Entry;

        let our_public_key = self.context.keystore.ton.public_key();

        // Use flag to update counter outside events map lock to reduce its duration
        let mut event_removed = false;

        // Handle only known TON events
        if let Entry::Occupied(entry) = self.ton_sol_events_state.pending.entry(account) {
            let remove_entry = || {
                // Remove pending event
                entry.remove();
                event_removed = true;
            };

            match event {
                // Remove event in confirmed state if the balance is not enough.
                //
                // NOTE: it is not strictly necessary to collect all signatures, so the
                // contract subscription is allowed to be dropped on nearly empty balance.
                //
                // This state can be achieved by calling `close` method on transfer contract
                // or execution `confirm` or `reject` after several years so that the cost of
                // keeping the contract almost nullifies its balance.
                (TonSolEvent::Closed, EventStatus::Confirmed) => remove_entry(),
                // Remove event if it was rejected
                (TonSolEvent::Rejected, _) | (_, EventStatus::Rejected) => remove_entry(),

                // Handle event initialization
                (TonSolEvent::ReceiveRoundRelays { keys }, _) => {
                    // Check if event contains our key
                    if keys.contains(our_public_key) {
                        // Start voting
                        self.spawn_background_task(
                            "update TON->SOL event",
                            self.clone().update_ton_sol_event(account),
                        );
                    } else {
                        remove_entry();
                    }
                }
                // Handle our confirmation or rejection
                (TonSolEvent::Confirm { public_key } | TonSolEvent::Reject { public_key }, _)
                    if public_key == our_public_key =>
                {
                    remove_entry();
                }
                _ => { /* Ignore other events */ }
            }
        }

        // Update metrics
        if event_removed {
            self.ton_sol_events_state
                .count
                .fetch_sub(1, Ordering::Release);
        }

        Ok(())
    }

    async fn process_btc_ton_event(
        self: Arc<Self>,
        (account, event): (UInt256, (BtcTonEvent, EventStatus)),
    ) -> Result<()> {
        use dashmap::mapref::entry::Entry;

        let our_public_key = self.context.keystore.ton.public_key();

        // Use flag to update counter outside events map lock to reduce its duration
        let mut event_removed = false;

        // Handle only known BTC events
        if let Entry::Occupied(entry) = self.btc_ton_events_state.pending.entry(account) {
            let remove_entry = || {
                // Remove pending event
                entry.remove();
                event_removed = true;
            };

            match event {
                // Remove event if voting process was finished
                (BtcTonEvent::Rejected, _)
                | (_, EventStatus::Confirmed | EventStatus::Rejected) => remove_entry(),
                // Handle event initialization
                (BtcTonEvent::ReceiveRoundRelays { keys }, _) => {
                    // Check if event contains our key
                    if keys.contains(our_public_key) {
                        // Start voting
                        self.spawn_background_task(
                            "update BTC->TON event",
                            self.clone().update_btc_ton_event(account),
                        );
                    } else {
                        remove_entry();
                    }
                }
                // Handle our confirmation or rejection
                (BtcTonEvent::Confirm { public_key } | BtcTonEvent::Reject { public_key }, _)
                    if public_key == our_public_key =>
                {
                    remove_entry()
                }
                _ => { /* Ignore other events */ }
            }
        }

        // Update metrics
        if event_removed {
            self.btc_ton_events_state
                .count
                .fetch_sub(1, Ordering::Release);
        }

        Ok(())
    }

    async fn process_ton_btc_event(
        self: Arc<Self>,
        (account, event): (UInt256, (TonBtcEvent, EventStatus)),
    ) -> Result<()> {
        use dashmap::mapref::entry::Entry;

        let our_public_key = self.context.keystore.ton.public_key();

        // Use flag to update counter outside events map lock to reduce its duration
        let mut event_removed = false;

        // Handle only known TON events
        if let Entry::Occupied(entry) = self.ton_btc_events_state.pending.entry(account) {
            let remove_entry = || {
                // Remove pending event
                entry.remove();
                event_removed = true;
            };

            match event {
                // Remove event if voting process was finished
                (TonBtcEvent::Rejected, _)
                | (_, EventStatus::Confirmed | EventStatus::Rejected) => {
                    remove_entry();

                    self.spawn_background_task(
                        "remove TON->BTC withdrawals",
                        self.clone().remove_ton_btc_withdrawals(account),
                    );
                }
                // Handle event initialization
                (TonBtcEvent::ReceiveRoundRelays { keys }, _) => {
                    // Check if event contains our key
                    if keys.contains(our_public_key) {
                        // Start voting
                        self.spawn_background_task(
                            "update TON->BTC event",
                            self.clone().update_ton_btc_event(account),
                        );
                    } else {
                        remove_entry();

                        self.spawn_background_task(
                            "remove TON->BTC withdrawals",
                            self.clone().remove_ton_btc_withdrawals(account),
                        );
                    }
                }
                // Handle our confirmation or rejection
                (
                    TonBtcEvent::Confirm { public_key }
                    | TonBtcEvent::Reject { public_key }
                    | TonBtcEvent::Cancel { public_key },
                    _,
                ) if public_key == our_public_key => {
                    remove_entry();

                    self.spawn_background_task(
                        "remove TON->BTC withdrawals",
                        self.clone().remove_ton_btc_withdrawals(account),
                    );
                }
                (TonBtcEvent::Withdrawal { withdrawal }, EventStatus::Pending) => {
                    self.spawn_background_task(
                        "add TON->BTC withdrawal",
                        self.clone().add_ton_btc_withdrawal(account, withdrawal),
                    );
                }
                _ => { /* Ignore other events */ }
            }
        }

        // Update metrics
        if event_removed {
            self.ton_btc_events_state
                .count
                .fetch_sub(1, Ordering::Release);
        }

        Ok(())
    }

    /// Check deployed event contract in parallel with transactions processing
    async fn preprocess_event<T: EventExt>(
        self: &Arc<Bridge>,
        account: UInt256,
        state: &EventsState<T>,
    ) -> Result<()> {
        // Wait contract state
        let ton_subscriber = &self.context.ton_subscriber;
        let contract = ton_subscriber.wait_contract_state(account).await?;
        let base_event_contract = EventBaseContract(&contract);

        // Check further steps based on event statuses
        match base_event_contract.process(
            self.context.keystore.ton.public_key(),
            T::REQUIRE_ALL_SIGNATURES,
            T::REQUIRE_COMMIT,
        )? {
            // Event was not activated yet, so it will be processed in
            // event transactions subscription
            EventAction::Nop => Ok(()),
            // Event was already processed, so just remove it
            // NOTE: it is ok to remove it even if it didn't exist
            EventAction::Remove => {
                state.remove(&account);
                Ok(())
            }
            // Start processing event.
            // NOTE: it is ok to update_ton_event twice because in fact it will
            // do anything only once
            EventAction::Vote | EventAction::Commit => T::update_event(self.clone(), account).await,
        }
    }

    async fn update_eth_ton_event(self: Arc<Self>, account: UInt256) -> Result<()> {
        if !self.eth_ton_events_state.start_processing(&account) {
            return Ok(());
        }

        let keystore = &self.context.keystore;
        let ton_subscriber = &self.context.ton_subscriber;
        let eth_subscribers = &self.context.eth_subscribers;

        // Wait contract state
        let contract = ton_subscriber.wait_contract_state(account).await?;

        match EventBaseContract(&contract).process(keystore.ton.public_key(), false, false)? {
            EventAction::Nop | EventAction::Commit => return Ok(()),
            EventAction::Remove => {
                self.eth_ton_events_state.remove(&account);
                return Ok(());
            }
            EventAction::Vote => { /* continue voting */ }
        }

        let event_init_data = EthTonEventContract(&contract).event_init_data()?;

        // Get event configuration data
        let data = {
            let state = self.state.read().await;
            state
                .eth_ton_event_configurations
                .get(&event_init_data.configuration)
                .map(|configuration| {
                    (
                        configuration.details.network_configuration.chain_id,
                        configuration.details.network_configuration.event_emitter,
                        configuration.event_abi.clone(),
                        configuration
                            .details
                            .network_configuration
                            .event_blocks_to_confirm,
                    )
                })
        };

        // NOTE: be sure to drop `eth_event_configurations` lock before that
        let (eth_subscriber, event_emitter, event_abi, blocks_to_confirm) = match data {
            // Configuration found
            Some((chain_id, event_emitter, abi, blocks_to_confirm)) => {
                // Get required subscriber
                match eth_subscribers.get_subscriber(chain_id) {
                    Some(subscriber) => (subscriber, event_emitter, abi, blocks_to_confirm),
                    None => {
                        tracing::error!(
                            event = %DisplayAddr(account),
                            chain_id,
                            "ETH->TON subscriber not found for event",
                        );
                        self.eth_ton_events_state.remove(&account);
                        return Ok(());
                    }
                }
            }
            // Configuration not found
            None => {
                tracing::error!(
                    event = %DisplayAddr(account),
                    configuration = %DisplayAddr(event_init_data.configuration),
                    "ETH->TON event configuration not found for event",
                );
                self.eth_ton_events_state.remove(&account);
                return Ok(());
            }
        };

        let account_addr = ton_block::MsgAddrStd::with_address(None, 0, account.into());

        // Verify ETH event and create message to event contract
        let message = match eth_subscriber
            .verify(
                event_init_data.vote_data,
                event_emitter,
                event_abi,
                blocks_to_confirm,
            )
            .await
        {
            // Confirm event if transaction was found
            Ok(VerificationStatus::Exists) => {
                UnsignedMessage::new(eth_ton_event_contract::confirm(), account).arg(account_addr)
            }
            // Reject event if transaction not found
            Ok(VerificationStatus::NotExists { reason }) => {
                tracing::warn!(
                    event = %DisplayAddr(account),
                    configuration = %DisplayAddr(event_init_data.configuration),
                    reason,
                    "rejecting ETH->TON event",
                );

                UnsignedMessage::new(eth_ton_event_contract::reject(), account).arg(account_addr)
            }
            // Skip event otherwise
            Err(e) => {
                tracing::error!(event = %DisplayAddr(account), "failed to verify ETH->TON event: {e:?}");
                self.eth_ton_events_state.remove(&account);
                return Ok(());
            }
        };

        // Clone events observer and deliver message to the contract
        let eth_ton_event_observer = match self.eth_ton_events_state.pending.get(&account) {
            Some(entry) => entry.observer.clone(),
            None => return Ok(()),
        };
        let eth_ton_events_state = Arc::downgrade(&self.eth_ton_events_state);

        self.context
            .deliver_message(
                eth_ton_event_observer,
                message,
                // Stop voting for the contract if it was removed
                move || match eth_ton_events_state.upgrade() {
                    Some(state) => state.pending.contains_key(&account),
                    None => false,
                },
            )
            .await?;
        Ok(())
    }

    async fn update_ton_eth_event(self: Arc<Self>, account: UInt256) -> Result<()> {
        if !self.ton_eth_events_state.start_processing(&account) {
            return Ok(());
        }

        let keystore = &self.context.keystore;
        let ton_subscriber = &self.context.ton_subscriber;

        // Wait contract state
        let contract = ton_subscriber.wait_contract_state(account).await?;
        let base_event_contract = EventBaseContract(&contract);

        // Check further steps based on event statuses
        match base_event_contract.process(keystore.ton.public_key(), true, false)? {
            EventAction::Nop | EventAction::Commit => return Ok(()),
            EventAction::Remove => {
                self.ton_eth_events_state.remove(&account);
                return Ok(());
            }
            EventAction::Vote => { /* continue voting */ }
        }
        let round_number = base_event_contract.round_number()?;

        // Get event details
        let event_init_data = TonEthEventContract(&contract).event_init_data()?;

        // Find suitable configuration
        // NOTE: be sure to drop `self.state` lock before removing pending ton event.
        // It may deadlock otherwise!
        let data = {
            let state = self.state.read().await;
            state
                .ton_eth_event_configurations
                .get(&event_init_data.configuration)
                .map(|configuration| {
                    (
                        configuration.details.network_configuration.proxy,
                        ton_abi::TokenValue::decode_params(
                            &configuration.event_abi,
                            event_init_data.vote_data.event_data.clone().into(),
                            &ton_abi::contract::ABI_VERSION_2_2,
                            false,
                        ),
                    )
                })
        };

        let decoded_data = match data {
            // Decode event data with event abi from configuration
            Some((proxy, data)) => data.and_then(|data| {
                Ok(make_mapped_ton_event(
                    event_init_data.vote_data.event_transaction_lt,
                    event_init_data.vote_data.event_timestamp,
                    map_ton_tokens_to_eth_bytes(data)?,
                    event_init_data.configuration,
                    account,
                    proxy,
                    round_number,
                ))
            }),
            // Do nothing when configuration was not found
            None => {
                tracing::error!(
                    event = %DisplayAddr(account),
                    configuration = %DisplayAddr(event_init_data.configuration),
                    "TON->ETH event configuration not found for event",
                );
                self.ton_eth_events_state.remove(&account);
                return Ok(());
            }
        };

        let account_addr = ton_block::MsgAddrStd::with_address(None, 0, account.into());

        let message = match decoded_data {
            // Confirm with signature
            Ok(data) => {
                tracing::info!(
                    event = %DisplayAddr(account),
                    data = hex::encode(&data),
                    "signing event data"
                );
                UnsignedMessage::new(ton_eth_event_contract::confirm(), account)
                    .arg(keystore.eth.sign(&data).to_vec())
                    .arg(account_addr)
            }

            // Reject if event data is invalid
            Err(e) => {
                tracing::warn!(
                    event = %DisplayAddr(account),
                    "failed to compute vote data signature: {e:?}",
                );
                UnsignedMessage::new(ton_eth_event_contract::reject(), account).arg(account_addr)
            }
        };

        // Clone events observer and deliver message to the contract
        let ton_eth_event_observer = match self.ton_eth_events_state.pending.get(&account) {
            Some(entry) => entry.observer.clone(),
            None => return Ok(()),
        };
        let ton_eth_events_state = Arc::downgrade(&self.ton_eth_events_state);

        self.context
            .deliver_message(
                ton_eth_event_observer,
                message,
                // Stop voting for the contract if it was removed
                move || match ton_eth_events_state.upgrade() {
                    Some(state) => state.pending.contains_key(&account),
                    None => false,
                },
            )
            .await?;
        Ok(())
    }

    async fn update_sol_ton_event(self: Arc<Self>, account: UInt256) -> Result<()> {
        let sol_subscriber = match &self.context.sol_subscriber {
            // Continue only of SOL subscriber is enabled and it is the first time we started processing this event
            Some(sol_subscriber) if self.sol_ton_events_state.start_processing(&account) => {
                sol_subscriber
            }
            _ => return Ok(()),
        };

        let keystore = &self.context.keystore;
        let ton_subscriber = &self.context.ton_subscriber;

        // Wait contract state
        let contract = ton_subscriber.wait_contract_state(account).await?;

        match EventBaseContract(&contract).process(keystore.ton.public_key(), false, false)? {
            EventAction::Nop | EventAction::Commit => return Ok(()),
            EventAction::Remove => {
                self.sol_ton_events_state.remove(&account);
                return Ok(());
            }
            EventAction::Vote => { /* continue voting */ }
        }

        let event_init_data = SolTonEventContract(&contract).event_init_data()?;

        // Find suitable configuration
        // NOTE: be sure to drop `self.state` lock before removing pending ton event.
        // It may deadlock otherwise!
        let data = {
            let state = self.state.read().await;
            state
                .sol_ton_event_configurations
                .get(&event_init_data.configuration)
                .map(|configuration| {
                    (
                        configuration.details.network_configuration.program,
                        TokenValue::decode_params(
                            &configuration.event_abi,
                            event_init_data.vote_data.event_data.clone().into(),
                            &ton_abi::contract::ABI_VERSION_2_2,
                            false,
                        ),
                    )
                })
        };

        let (program, decoded_event_data) = match data {
            // Decode event data with event abi from configuration
            Some((program, data)) => (
                program,
                data.and_then(|data| eth_ton_abi_converter::borsh::serialize(&data))?,
            ),
            // Do nothing when configuration was not found
            None => {
                tracing::error!(
                    event = %DisplayAddr(account),
                    configuration = %DisplayAddr(event_init_data.configuration),
                    "SOL->TON event configuration not found for event",
                );
                self.sol_ton_events_state.remove(&account);
                return Ok(());
            }
        };

        let signature =
            solana_sdk::signature::Signature::from_str(&event_init_data.vote_data.signature)?;

        let program_id = Pubkey::new_from_array(program.inner());

        let transaction_data = SolTonTransactionData {
            program_id,
            signature,
            slot: event_init_data.vote_data.slot,
            block_time: event_init_data.vote_data.block_time as i64,
            seed: event_init_data.vote_data.account_seed,
        };

        let account_data = SolTonAccountData {
            program_id,
            seed: event_init_data.vote_data.account_seed,
            event_data: decoded_event_data,
        };

        let account_addr = ton_block::MsgAddrStd::with_address(None, 0, account.into());

        // Verify SOL->TON event and create message to event contract
        let message = match sol_subscriber
            .verify_sol_ton_event(transaction_data, account_data)
            .await
        {
            // Confirm event if transaction was found
            Ok(VerificationStatus::Exists) => {
                UnsignedMessage::new(sol_ton_event_contract::confirm(), account).arg(account_addr)
            }
            // Reject event if transaction not found
            Ok(VerificationStatus::NotExists { reason }) => {
                tracing::warn!(
                    event = %DisplayAddr(account),
                    configuration = %DisplayAddr(event_init_data.configuration),
                    reason,
                    "rejecting SOL->TON event",
                );

                UnsignedMessage::new(sol_ton_event_contract::reject(), account).arg(account_addr)
            }
            // Skip event otherwise
            Err(e) => {
                tracing::error!(
                    event = %DisplayAddr(account),
                    "failed to verify SOL->TON event: {e:?}",
                );
                self.sol_ton_events_state.remove(&account);
                return Ok(());
            }
        };

        // Clone events observer and deliver message to the contract
        let sol_ton_event_observer = match self.sol_ton_events_state.pending.get(&account) {
            Some(entry) => entry.observer.clone(),
            None => return Ok(()),
        };
        let sol_ton_events_state = Arc::downgrade(&self.sol_ton_events_state);

        self.context
            .deliver_message(
                sol_ton_event_observer,
                message,
                // Stop voting for the contract if it was removed
                move || match sol_ton_events_state.upgrade() {
                    Some(state) => state.pending.contains_key(&account),
                    None => false,
                },
            )
            .await?;

        Ok(())
    }

    async fn update_ton_sol_event(self: Arc<Self>, account: UInt256) -> Result<()> {
        let sol_subscriber = match &self.context.sol_subscriber {
            // Continue only of SOL subscriber is enabled and it is the first time we started processing this event
            Some(sol_subscriber) if self.ton_sol_events_state.start_processing(&account) => {
                sol_subscriber
            }
            _ => return Ok(()),
        };

        let keystore = &self.context.keystore;
        let ton_subscriber = &self.context.ton_subscriber;

        // Wait contract state
        let contract = ton_subscriber.wait_contract_state(account).await?;
        let base_event_contract = EventBaseContract(&contract);

        // Check further steps based on event statuses
        match base_event_contract.process(keystore.ton.public_key(), false, false)? {
            EventAction::Nop | EventAction::Commit => return Ok(()),
            EventAction::Remove => {
                self.ton_sol_events_state.remove(&account);
                return Ok(());
            }
            EventAction::Vote => { /* continue voting */ }
        }
        let round_number = base_event_contract.round_number()?;

        // Get event details
        let event_init_data = TonSolEventContract(&contract).event_init_data()?;

        // Find suitable configuration
        // NOTE: be sure to drop `self.state` lock before removing pending ton event.
        // It may deadlock otherwise!
        let data = {
            let state = self.state.read().await;
            state
                .ton_sol_event_configurations
                .get(&event_init_data.configuration)
                .map(|configuration| {
                    (
                        configuration.details.network_configuration.program,
                        configuration.details.network_configuration.instruction,
                        configuration.details.network_configuration.execute_needed,
                        configuration
                            .details
                            .network_configuration
                            .execute_instruction,
                        configuration
                            .details
                            .network_configuration
                            .execute_payload_instruction,
                        TokenValue::decode_params(
                            &configuration.event_abi,
                            event_init_data.vote_data.event_data.clone().into(),
                            &ton_abi::contract::ABI_VERSION_2_2,
                            false,
                        ),
                    )
                })
        };

        let (
            program_id,
            instruction,
            execute_needed,
            execute_instruction,
            execute_payload_instruction,
            decoded_event_data,
        ) = match data {
            // Decode event data with event abi from configuration
            Some((
                program,
                instruction,
                execute_needed,
                execute_instruction,
                execute_payload_instruction,
                data,
            )) => (
                Pubkey::new_from_array(program.inner()),
                instruction,
                execute_needed,
                execute_instruction,
                execute_payload_instruction,
                data.and_then(|data| eth_ton_abi_converter::borsh::serialize(&data))?,
            ),
            // Do nothing when configuration was not found
            None => {
                tracing::error!(
                    event = %DisplayAddr(account),
                    configuration = %DisplayAddr(event_init_data.configuration),
                    "TON->SOL event configuration not found for event",
                );
                self.ton_sol_events_state.remove(&account);
                return Ok(());
            }
        };

        let event_configuration = Pubkey::new_from_array(event_init_data.configuration.inner());
        let event_data = solana_sdk::hash::hash(&decoded_event_data);

        let proposal_pubkey = solana_bridge::bridge_helper::get_associated_proposal_address(
            &program_id,
            round_number,
            event_init_data.vote_data.event_timestamp,
            event_init_data.vote_data.event_transaction_lt,
            &event_configuration,
            &event_data.to_bytes(),
        );

        let voter_pubkey = self.context.keystore.sol.public_key();

        let account_addr = ton_block::MsgAddrStd::with_address(None, 0, account.into());

        let (sol_message_vote, sol_message_execute, sol_message_execute_payload, ton_message) =
            match sol_subscriber
                .verify_ton_sol_event(proposal_pubkey, decoded_event_data)
                .await
            {
                // Confirm event if transaction was found
                Ok(VerificationStatus::Exists) => {
                    let vote_ix = solana_bridge::instructions::vote_for_proposal_ix(
                        program_id,
                        instruction,
                        &voter_pubkey,
                        &proposal_pubkey,
                        round_number,
                        solana_bridge::bridge_types::Vote::Confirm,
                    );

                    let sol_message_vote =
                        solana_sdk::message::Message::new(&[vote_ix], Some(&voter_pubkey));

                    let mut sol_message_execute = None;
                    let mut sol_message_execute_payload = None;
                    if execute_needed {
                        let accounts = event_init_data
                            .vote_data
                            .execute_accounts
                            .into_iter()
                            .map(|account| {
                                (
                                    Pubkey::new_from_array(account.account.inner()),
                                    account.read_only,
                                    account.is_signer,
                                )
                            })
                            .collect();

                        let execute_ix = solana_bridge::instructions::execute_proposal_ix(
                            program_id,
                            execute_instruction,
                            proposal_pubkey,
                            accounts,
                        );

                        sol_message_execute = Some(solana_sdk::message::Message::new(
                            &[execute_ix],
                            Some(&voter_pubkey),
                        ));

                        if event_init_data.vote_data.execute_payload_needed {
                            let accounts = event_init_data
                                .vote_data
                                .execute_payload_accounts
                                .into_iter()
                                .map(|account| {
                                    (
                                        Pubkey::new_from_array(account.account.inner()),
                                        account.read_only,
                                        account.is_signer,
                                    )
                                })
                                .collect();

                            let execute_ix = solana_bridge::instructions::execute_payload_ix(
                                program_id,
                                execute_payload_instruction,
                                proposal_pubkey,
                                accounts,
                            );

                            sol_message_execute_payload = Some(solana_sdk::message::Message::new(
                                &[execute_ix],
                                Some(&voter_pubkey),
                            ));
                        }
                    }

                    let ton_message =
                        UnsignedMessage::new(ton_sol_event_contract::confirm(), account)
                            .arg(account_addr);

                    (
                        sol_message_vote,
                        sol_message_execute,
                        sol_message_execute_payload,
                        ton_message,
                    )
                }
                Ok(VerificationStatus::NotExists { reason }) => {
                    tracing::warn!(
                        event = %DisplayAddr(account),
                        configuration = %DisplayAddr(event_init_data.configuration),
                        reason,
                        "rejecting TON->SOL event",
                    );

                    let ix = solana_bridge::instructions::vote_for_proposal_ix(
                        program_id,
                        instruction,
                        &voter_pubkey,
                        &proposal_pubkey,
                        round_number,
                        solana_bridge::bridge_types::Vote::Reject,
                    );
                    let sol_message = solana_sdk::message::Message::new(&[ix], Some(&voter_pubkey));

                    let ton_message =
                        UnsignedMessage::new(ton_sol_event_contract::reject(), account)
                            .arg(account_addr);

                    (sol_message, None, None, ton_message)
                }
                // Skip event otherwise
                Err(e) => {
                    tracing::error!(
                        event = %DisplayAddr(account),
                        "failed to verify TON->SOL event: {e:?}",
                    );
                    self.ton_sol_events_state.remove(&account);
                    return Ok(());
                }
            };

        if !sol_subscriber
            .is_already_voted(round_number, &proposal_pubkey, &voter_pubkey)
            .await?
        {
            // Extract vote and log it
            let c_ix = sol_message_vote.instructions.first().trust_me();
            let ix = solana_bridge::instructions::VoteForProposal::try_from_slice(&c_ix.data)?;

            tracing::info!(
                vote = ?ix.vote,
                %proposal_pubkey,
                "sending a vote for Solana proposal",
            );

            // Send confirm/reject to Solana
            sol_subscriber
                .send_message(sol_message_vote, &self.context.keystore)
                .await
                .map_err(parse_client_error)?;

            // Execute proposal
            if let Some(message) = sol_message_execute {
                tracing::info!(
                    %proposal_pubkey,
                    "execute proposal",
                );

                if let Err(e) = sol_subscriber
                    .send_message(message, &self.context.keystore)
                    .await
                    .map_err(parse_client_error)
                {
                    tracing::error!(
                        %proposal_pubkey,
                        "failed to execute solana proposal: {e:?}",
                    );
                }
            }

            // Execute payload
            if let Some(message) = sol_message_execute_payload {
                tracing::info!(
                    %proposal_pubkey,
                    "execute payload",
                );

                if let Err(e) = sol_subscriber
                    .send_message(message, &self.context.keystore)
                    .await
                    .map_err(parse_client_error)
                {
                    tracing::error!(
                        %proposal_pubkey,
                        "failed to execute solana payload: {e:?}",
                    );
                }
            }
        }

        // Clone events observer and deliver message to the contract
        let ton_sol_event_observer = match self.ton_sol_events_state.pending.get(&account) {
            Some(entry) => entry.observer.clone(),
            None => return Ok(()),
        };
        let ton_sol_events_state = Arc::downgrade(&self.ton_sol_events_state);

        self.context
            .deliver_message(
                ton_sol_event_observer,
                ton_message,
                // Stop voting for the contract if it was removed
                move || match ton_sol_events_state.upgrade() {
                    Some(state) => state.pending.contains_key(&account),
                    None => false,
                },
            )
            .await?;

        Ok(())
    }

    async fn update_btc_ton_event(self: Arc<Self>, account: UInt256) -> Result<()> {
        let btc_subscriber = match &self.context.btc_subscriber {
            // Continue only of BTC subscriber is enabled and it is the first time we started processing this event
            Some(btc_subscriber) if self.btc_ton_events_state.start_processing(&account) => {
                btc_subscriber
            }
            _ => return Ok(()),
        };

        let keystore = &self.context.keystore;
        let ton_subscriber = &self.context.ton_subscriber;

        // Wait contract state
        let contract = ton_subscriber.wait_contract_state(account).await?;

        match EventBaseContract(&contract).process(keystore.ton.public_key(), false, false)? {
            EventAction::Nop | EventAction::Commit => return Ok(()),
            EventAction::Remove => {
                self.btc_ton_events_state.remove(&account);
                return Ok(());
            }
            EventAction::Vote => { /* continue voting */ }
        };

        let event_init_data = BtcTonEventContract(&contract).event_init_data()?;

        // Find suitable configuration
        // NOTE: be sure to drop `self.state` lock before removing pending ton event.
        // It may deadlock otherwise!
        let data = {
            let state = self.state.read().await;
            state
                .btc_ton_event_configurations
                .get(&event_init_data.configuration)
                .map(|configuration| {
                    (
                        configuration.details.network_configuration.proxy,
                        configuration
                            .details
                            .network_configuration
                            .event_blocks_to_confirm,
                    )
                })
        };

        let (proxy, blocks_to_confirm) = match data {
            // Decode event data with event abi from configuration
            Some((proxy, blocks_to_confirm)) =>
                (proxy, blocks_to_confirm)
            ,
            // Do nothing when configuration was not found
            None => {
                tracing::error!(
                    event = %DisplayAddr(account),
                    configuration = %DisplayAddr(event_init_data.configuration),
                    "BTC->TON event configuration not found for event",
                );
                self.btc_ton_events_state.remove(&account);
                return Ok(());
            }
        };

        // Get deposit account
        let proxy_contract = ton_subscriber.wait_contract_state(proxy).await?;
        let deposit = BtcProxyContract(&proxy_contract)
            .get_deposit_account(&event_init_data.vote_data.beneficiary)?;

        // Get deposit account id
        let deposit_contract = ton_subscriber.wait_contract_state(deposit).await?;
        let deposit_account_id = BtcDepositContract(&deposit_contract).get_account_id()?;

        let account_addr = ton_block::MsgAddrStd::with_address(None, 0, account.into());

        // Verify BTC->TON event and create message to event contract
        let message = match btc_subscriber
            .verify_btc_ton_event(
                account,
                blocks_to_confirm,
                deposit_account_id,
                event_init_data.vote_data,
            )
            .await
        {
            // Confirm event if transaction was found
            Ok(VerificationStatus::Exists) => {
                UnsignedMessage::new(btc_ton_event_contract::confirm(), account).arg(account_addr)
            }
            // Reject event if transaction not found
            Ok(VerificationStatus::NotExists { reason }) => {
                tracing::warn!(
                    event = %DisplayAddr(account),
                    configuration = %DisplayAddr(event_init_data.configuration),
                    reason,
                    "rejecting BTC->TON event",
                );

                UnsignedMessage::new(btc_ton_event_contract::reject(), account).arg(account_addr)
            }
            // Skip event otherwise
            Err(e) => {
                tracing::error!(event = %DisplayAddr(account),"failed to verify BTC->TON event: {e:?}",);
                self.btc_ton_events_state.remove(&account);
                return Ok(());
            }
        };

        // Clone events observer and deliver message to the contract
        let btc_ton_event_observer = match self.btc_ton_events_state.pending.get(&account) {
            Some(entry) => entry.observer.clone(),
            None => return Ok(()),
        };
        let btc_ton_events_state = Arc::downgrade(&self.btc_ton_events_state);

        self.context
            .deliver_message(
                btc_ton_event_observer,
                message,
                // Stop voting for the contract if it was removed
                move || match btc_ton_events_state.upgrade() {
                    Some(state) => state.pending.contains_key(&account),
                    None => false,
                },
            )
            .await?;

        Ok(())
    }

    async fn commit_btc_ton_event(self: Arc<Self>, account: UInt256) -> Result<()> {
        let btc_subscriber = match &self.context.btc_subscriber {
            Some(btc_subscriber) => btc_subscriber,
            _ => return Ok(()),
        };

        let keystore = &self.context.keystore;
        let ton_subscriber = &self.context.ton_subscriber;

        // Wait contract state
        let contract = ton_subscriber.wait_contract_state(account).await?;

        match EventBaseContract(&contract).process(keystore.ton.public_key(), false, true)? {
            EventAction::Nop | EventAction::Vote => return Ok(()),
            EventAction::Remove => {
                self.btc_ton_events_state.remove(&account);
                return Ok(());
            }
            EventAction::Commit => { /* continue committing */ }
        };

        let event_init_data = BtcTonEventContract(&contract).event_init_data()?;

        // Find suitable configuration
        // NOTE: be sure to drop `self.state` lock before removing pending ton event.
        // It may deadlock otherwise!
        let data = {
            let state = self.state.read().await;
            state
                .btc_ton_event_configurations
                .get(&event_init_data.configuration)
                .map(|configuration| configuration.details.network_configuration.proxy)
        };

        let proxy = match data {
            // Decode event data with event abi from configuration
            Some(proxy) => proxy,
            // Do nothing when configuration was not found
            None => {
                tracing::error!(
                    event = %DisplayAddr(account),
                    configuration = %DisplayAddr(event_init_data.configuration),
                    "BTC->TON event configuration not found for event",
                );
                self.btc_ton_events_state.remove(&account);
                return Ok(());
            }
        };

        // Get deposit account
        let proxy_contract = ton_subscriber.wait_contract_state(proxy).await?;
        let deposit = BtcProxyContract(&proxy_contract)
            .get_deposit_account(&event_init_data.vote_data.beneficiary)?;

        // Get deposit account id
        let deposit_contract = ton_subscriber.wait_contract_state(deposit).await?;
        let deposit_account_id = BtcDepositContract(&deposit_contract).get_account_id()?;

        // Commit deposit
        if let Err(e) = btc_subscriber
            .commit(deposit_account_id, event_init_data.vote_data)
            .await
        {
            tracing::error!(event = %DisplayAddr(account),"failed to commit BTC->TON event: {e:?}",);
        }

        // Remove subscription anyway
        self.btc_ton_events_state.remove(&account);

        Ok(())
    }

    async fn update_ton_btc_event(self: Arc<Self>, account: UInt256) -> Result<()> {
        let btc_subscriber = match &self.context.btc_subscriber {
            // Continue only of BTC subscriber is enabled and it is the first time we started processing this event
            Some(btc_subscriber) if self.btc_ton_events_state.start_processing(&account) => {
                btc_subscriber
            }
            _ => return Ok(()),
        };

        let keystore = &self.context.keystore;
        let ton_subscriber = &self.context.ton_subscriber;

        // Wait contract state
        let contract = ton_subscriber.wait_contract_state(account).await?;

        let event_action =
            EventBaseContract(&contract).process(keystore.ton.public_key(), true, false)?;
        match event_action {
            EventAction::Nop | EventAction::Commit => return Ok(()),
            EventAction::Remove => {
                self.btc_ton_events_state.remove(&account);
                return Ok(());
            }
            EventAction::Vote => { /* continue voting */ }
        }

        let account_addr = ton_block::MsgAddrStd::with_address(None, 0, account.into());

        // Get BTC transaction
        let message = match btc_subscriber.create_btc_transaction(account).await {
            Ok(transaction) => UnsignedMessage::new(ton_btc_event_contract::confirm(), account)
                .arg(bitcoin::consensus::serialize(&transaction))
                .arg(account_addr),
            // Skip event otherwise
            Err(e) => {
                tracing::error!(event = %DisplayAddr(account),"failed to create BTC transaction: {e:?}",);
                self.ton_btc_events_state.remove(&account);
                return Ok(());
            }
        };

        // Clone events observer and deliver message to the contract
        let ton_btc_event_observer = match self.ton_btc_events_state.pending.get(&account) {
            Some(entry) => entry.observer.clone(),
            None => return Ok(()),
        };
        let ton_btc_events_state = Arc::downgrade(&self.ton_btc_events_state);

        self.context
            .deliver_message(
                ton_btc_event_observer,
                message,
                // Stop voting for the contract if it was removed
                move || match ton_btc_events_state.upgrade() {
                    Some(state) => state.pending.contains_key(&account),
                    None => false,
                },
            )
            .await?;

        Ok(())
    }

    async fn commit_ton_btc_event(self: Arc<Self>, account: UInt256) -> Result<()> {
        Ok(())
    }

    async fn add_ton_btc_withdrawal(
        self: Arc<Self>,
        account: UInt256,
        withdrawal: BtcWithdrawal,
    ) -> Result<()> {
        let btc_subscriber = match &self.context.btc_subscriber {
            Some(btc_subscriber) => btc_subscriber,
            _ => return Ok(()),
        };

        let id = bitcoin::PublicKey::from_slice(&withdrawal.recipient)
            .map(|pk| bitcoin::Script::new_p2pk(&pk))?;

        btc_subscriber
            .add_pending_withdrawal(account, id, withdrawal.amount)
            .await?;

        Ok(())
    }

    async fn remove_ton_btc_withdrawals(self: Arc<Self>, account: UInt256) -> Result<()> {
        let btc_subscriber = match &self.context.btc_subscriber {
            Some(btc_subscriber) => btc_subscriber,
            _ => return Ok(()),
        };

        btc_subscriber.remove_pending_withdrawals(account).await?;

        Ok(())
    }

    async fn check_connector_contract(&self, connector_account: UInt256) -> Result<()> {
        let ton_subscriber = &self.context.ton_subscriber;

        // Get event configuration address
        let event_configuration = {
            // Wait until connector contract state is found
            let contract = ton_subscriber
                .wait_contract_state(connector_account)
                .await?;

            // Extract details
            let connector_details = ConnectorContract(&contract).get_details()?;
            tracing::info!(
                connector = %DisplayAddr(connector_account),
                ?connector_details,
                "got connector details",
            );

            // Do nothing if it is disabled
            if !connector_details.enabled {
                return Ok(());
            }
            connector_details.event_configuration
        };

        // Wait until event configuration state is found
        let contract = ton_subscriber
            .wait_contract_state(event_configuration)
            .await?;
        tracing::info!(
            configuration = %DisplayAddr(event_configuration),
            connector = %DisplayAddr(connector_account),
            "got configuration contract",
        );

        // Extract and process info from contract
        let mut state = self.state.write().await;
        self.process_event_configuration(
            &mut state,
            &connector_account,
            &event_configuration,
            &contract,
        )?;

        Ok(())
    }

    async fn get_all_configurations(&self) -> Result<()> {
        // Lock state before other logic to make sure that all events
        // will be queued in their handlers
        let mut state = self.state.write().await;

        let shard_accounts = self.context.get_all_shard_accounts().await?;

        let ton_subscriber = &self.context.ton_subscriber;

        let contract = shard_accounts
            .find_account(&self.bridge_account)?
            .ok_or(BridgeError::BridgeAccountNotFound)?;
        let bridge = BridgeContract(&contract);

        let connector_count = bridge
            .connector_counter()
            .context("Failed to get connector count")?;

        // Iterate for all connectors
        for id in 0..connector_count {
            // Compute next connector address
            let connector_account = bridge
                .derive_connector_address(id)
                .context("Failed to derive connector address")?;

            // Extract details from contract
            let details = match shard_accounts.find_account(&connector_account)? {
                Some(contract) => match ConnectorContract(&contract).get_details() {
                    Ok(details) => details,
                    Err(e) => {
                        tracing::error!(
                            connector = %DisplayAddr(connector_account),
                            "failed to get connector details: {e:?}",
                        );
                        continue;
                    }
                },
                None => {
                    tracing::error!(
                        connector = %DisplayAddr(connector_account),
                        "connector not found",
                    );
                    continue;
                }
            };
            tracing::info!(
                connector = %DisplayAddr(connector_account),
                id,
                ?details,
                "found configuration connector",
            );

            let enabled = details.enabled;
            let configuration_account = details.event_configuration;

            let observer = AccountObserver::new(&self.connectors_tx);

            // Add new connector
            state.connectors.insert(connector_account, observer.clone());

            // Subscribe connector for transaction
            ton_subscriber.add_transactions_subscription([connector_account], &observer);

            // Skip event configuration if it is disabled
            if !enabled {
                continue;
            }

            // Find event configuration contract
            let configuration_contract =
                match shard_accounts.find_account(&configuration_account)? {
                    Some(contract) => contract,
                    None => {
                        // It is a strange situation when connector contains an address of the contract
                        // which doesn't exist, so log it here to investigate it later
                        tracing::warn!(
                            connector = %DisplayAddr(connector_account),
                            configuration = %DisplayAddr(details.event_configuration),
                            "connected configuration not found",
                        );
                        continue;
                    }
                };

            // Add event configuration
            if let Err(e) = self.process_event_configuration(
                &mut state,
                &connector_account,
                &configuration_account,
                &configuration_contract,
            ) {
                tracing::error!(
                    connector = %DisplayAddr(connector_account),
                    configuration = %DisplayAddr(details.event_configuration),
                    "failed to process event configuration: {e:?}",
                );
            }
        }

        // Done
        Ok(())
    }

    /// Searches for the account contract and extracts the configuration information into context
    fn process_event_configuration(
        &self,
        state: &mut BridgeState,
        connector_account: &UInt256,
        configuration_account: &UInt256,
        configuration_contract: &ExistingContract,
    ) -> Result<()> {
        // Get event type using base contract abi
        let event_type = EventConfigurationBaseContract(configuration_contract)
            .get_type()
            .context("Failed to get event configuration type")?;
        tracing::info!(
            connector = %DisplayAddr(connector_account),
            configuration = %DisplayAddr(configuration_account),
            ?event_type,
            "found new configuration contract"
        );

        if !state.connectors.contains_key(connector_account) {
            return Err(BridgeError::UnknownConnector.into());
        }

        match event_type {
            // Extract and populate ETH->TON event configuration details
            EventType::EthTon => self
                .add_eth_ton_event_configuration(
                    state,
                    configuration_account,
                    configuration_contract,
                )
                .context("Failed to add ETH->TON event configuration")?,
            // Extract and populate TON->ETH event configuration details
            EventType::TonEth => self
                .add_ton_eth_event_configuration(
                    state,
                    configuration_account,
                    configuration_contract,
                )
                .context("Failed to add TON->ETH event configuration")?,
            // Extract and populate SOL->TON event configuration details
            EventType::SolTon => self
                .add_sol_ton_event_configuration(
                    state,
                    configuration_account,
                    configuration_contract,
                )
                .context("Failed to add SOL->TON event configuration")?,
            // Extract and populate TON->SOL event configuration details
            EventType::TonSol => self
                .add_ton_sol_event_configuration(
                    state,
                    configuration_account,
                    configuration_contract,
                )
                .context("Failed to add TON->SOL event configuration")?,
            // Extract and populate BTC->TON event configuration details
            EventType::BtcTon => self
                .add_btc_ton_event_configuration(
                    state,
                    configuration_account,
                    configuration_contract,
                )
                .context("Failed to add BTC->TON event configuration")?,
            // Extract and populate TON->BTC event configuration details
            EventType::TonBtc => self
                .add_ton_btc_event_configuration(
                    state,
                    configuration_account,
                    configuration_contract,
                )
                .context("Failed to add TON->BTC event configuration")?,
        };

        // Done
        Ok(())
    }

    fn add_eth_ton_event_configuration(
        &self,
        state: &mut BridgeState,
        account: &UInt256,
        contract: &ExistingContract,
    ) -> Result<()> {
        let flags = EventConfigurationBaseContract(contract)
            .get_flags()
            .context("Failed to get ETH->TON event configuration flags")?;

        // Get configuration details
        let details = EthTonEventConfigurationContract(contract)
            .get_details()
            .context("Failed to get ETH->TON event configuration details")?;

        let ctx = flags
            .map(|flags| EthToTonMappingContext::from(flags as u8))
            .unwrap_or_default();

        // Verify and prepare abi
        let event_abi = Arc::new(EthEventAbi::new(
            &details.basic_configuration.event_abi,
            ctx,
        )?);
        let topic_hash = event_abi.get_eth_topic_hash().to_fixed_bytes();
        let eth_contract_address = details.network_configuration.event_emitter;

        // Get suitable ETH subscriber for specified chain id
        let eth_subscriber = self
            .context
            .eth_subscribers
            .get_subscriber(details.network_configuration.chain_id)
            .ok_or(BridgeError::UnknownChainId)?;

        // Add unique event hash
        add_event_code_hash(
            &mut state.event_code_hashes,
            &details.basic_configuration.event_code,
            EventType::EthTon,
        )?;

        // Add configuration entry
        let observer = AccountObserver::new(&self.eth_ton_event_configurations_tx);
        match state.eth_ton_event_configurations.entry(*account) {
            hash_map::Entry::Vacant(entry) => {
                tracing::info!(
                    configuration = %DisplayAddr(account),
                    ?details, "added new ETH->TON event configuration"
                );

                self.total_active_eth_ton_event_configurations
                    .fetch_add(1, Ordering::Release);

                entry.insert(EthTonEventConfigurationState {
                    details,
                    event_abi,
                    _observer: observer.clone(),
                });
            }
            hash_map::Entry::Occupied(_) => {
                tracing::info!(
                    configuration = %DisplayAddr(account),
                    "ETH->TON event configuration already exists",
                );
                return Err(BridgeError::EventConfigurationAlreadyExists.into());
            }
        };

        // Subscribe to ETH events
        eth_subscriber.subscribe(eth_contract_address.into(), topic_hash, *account);

        // Subscribe to TON events
        self.context
            .ton_subscriber
            .add_transactions_subscription([*account], &observer);

        // Done
        Ok(())
    }

    fn add_ton_eth_event_configuration(
        &self,
        state: &mut BridgeState,
        account: &UInt256,
        contract: &ExistingContract,
    ) -> Result<()> {
        // Get configuration details
        let details = TonEthEventConfigurationContract(contract)
            .get_details()
            .context("Failed to get TON->ETH event configuration details")?;

        // Check if configuration is expired
        let current_timestamp = self.context.ton_subscriber.current_utime();
        if details.is_expired(current_timestamp) {
            // Do nothing in that case
            tracing::warn!(
                configuration = %DisplayAddr(account),
                current_timestamp,
                end_timestamp = details.network_configuration.end_timestamp,
                "ignoring disabled TON->ETH event configuration",
            );
            return Ok(());
        };

        // Verify and prepare abi
        let event_abi = decode_ton_event_abi(&details.basic_configuration.event_abi)?;

        // Add unique event hash
        add_event_code_hash(
            &mut state.event_code_hashes,
            &details.basic_configuration.event_code,
            EventType::TonEth,
        )?;

        // Add configuration entry
        let observer = AccountObserver::new(&self.ton_eth_event_configurations_tx);
        match state.ton_eth_event_configurations.entry(*account) {
            hash_map::Entry::Vacant(entry) => {
                tracing::info!(
                    configuration = %DisplayAddr(account),
                    ?details,
                    "added new TON->ETH event configuration"
                );

                self.total_active_ton_eth_event_configurations
                    .fetch_add(1, Ordering::Release);

                entry.insert(TonEthEventConfigurationState {
                    details,
                    event_abi,
                    _observer: observer.clone(),
                });
            }
            hash_map::Entry::Occupied(_) => {
                tracing::info!(
                    configuration = %DisplayAddr(account),
                    "TON->ETH event configuration already exists",
                );
                return Err(BridgeError::EventConfigurationAlreadyExists.into());
            }
        };

        // Subscribe to TON events
        self.context
            .ton_subscriber
            .add_transactions_subscription([*account], &observer);

        // Done
        Ok(())
    }

    fn add_sol_ton_event_configuration(
        &self,
        state: &mut BridgeState,
        account: &UInt256,
        contract: &ExistingContract,
    ) -> Result<()> {
        if self.context.sol_subscriber.is_none() {
            tracing::info!(
                configuration = %DisplayAddr(account),
                "ignoring SOL->TON event configuration: Solana subscriber is disabled",
            );
            return Ok(());
        }

        // Get configuration details
        let details = SolTonEventConfigurationContract(contract)
            .get_details()
            .context("Failed to get SOL->TON event configuration details")?;

        // Check if configuration is expired
        let current_timestamp = self.context.ton_subscriber.current_utime();
        if details.is_expired(current_timestamp as u64) {
            // Do nothing in that case
            tracing::warn!(
                configuration = %DisplayAddr(account),
                current_timestamp,
                end_timestamp = details.network_configuration.end_timestamp,
                "ignoring disabled SOL->TON event configuration",
            );
            return Ok(());
        };

        // Verify and prepare abi
        let event_abi = decode_ton_event_abi(&details.basic_configuration.event_abi)?;

        // Add unique event hash
        add_event_code_hash(
            &mut state.event_code_hashes,
            &details.basic_configuration.event_code,
            EventType::SolTon,
        )?;

        // Add configuration entry
        let observer = AccountObserver::new(&self.sol_ton_event_configurations_tx);
        match state.sol_ton_event_configurations.entry(*account) {
            hash_map::Entry::Vacant(entry) => {
                tracing::info!(
                    configuration = %DisplayAddr(account),
                    ?details,
                    "added new SOl->TON event configuration",
                );

                self.total_active_sol_ton_event_configurations
                    .fetch_add(1, Ordering::Release);

                entry.insert(SolTonEventConfigurationState {
                    details,
                    event_abi,
                    _observer: observer.clone(),
                });
            }
            hash_map::Entry::Occupied(_) => {
                tracing::info!(
                    configuration = %DisplayAddr(account),
                    "SOl->TON event configuration already exists",
                );
                return Err(BridgeError::EventConfigurationAlreadyExists.into());
            }
        };

        // Subscribe to TON events
        self.context
            .ton_subscriber
            .add_transactions_subscription([*account], &observer);

        // Done
        Ok(())
    }

    fn add_ton_sol_event_configuration(
        &self,
        state: &mut BridgeState,
        account: &UInt256,
        contract: &ExistingContract,
    ) -> Result<()> {
        let sol_subscriber = match &self.context.sol_subscriber {
            Some(sol_subscriber) => sol_subscriber,
            None => {
                tracing::info!(
                    configuration = %DisplayAddr(account),
                    "ignoring TON->SOL event configuration: Solana subscriber is disabled",
                );
                return Ok(());
            }
        };

        // Get configuration details
        let details = TonSolEventConfigurationContract(contract)
            .get_details()
            .context("Failed to get TON->SOL event configuration details")?;

        // Check if configuration is expired
        let current_timestamp = self.context.ton_subscriber.current_utime();
        if details.is_expired(current_timestamp) {
            // Do nothing in that case
            tracing::warn!(
                configuration = %DisplayAddr(account),
                current_timestamp,
                end_timestamp = details.network_configuration.end_timestamp,
                "ignoring disabled TON->SOL event configuration",
            );
            return Ok(());
        };

        // Verify and prepare abi
        let event_abi = decode_ton_event_abi(&details.basic_configuration.event_abi)?;

        // Add unique event hash
        add_event_code_hash(
            &mut state.event_code_hashes,
            &details.basic_configuration.event_code,
            EventType::TonSol,
        )?;

        // Get solana program address to subscribe
        let program_pubkey = Pubkey::new_from_array(details.network_configuration.program.inner());

        // Add configuration entry
        let observer = AccountObserver::new(&self.ton_sol_event_configurations_tx);
        match state.ton_sol_event_configurations.entry(*account) {
            hash_map::Entry::Vacant(entry) => {
                tracing::info!(
                    configuration = %DisplayAddr(account),
                    ?details,
                    "added new TON->SOL event configuration",
                );

                self.total_active_ton_sol_event_configurations
                    .fetch_add(1, Ordering::Release);

                entry.insert(TonSolEventConfigurationState {
                    details,
                    event_abi,
                    _observer: observer.clone(),
                });
            }
            hash_map::Entry::Occupied(_) => {
                tracing::info!(
                    configuration = %DisplayAddr(account),
                    "TON->SOL event configuration already exists"
                );
                return Err(BridgeError::EventConfigurationAlreadyExists.into());
            }
        };

        // Subscribe to Solana programs
        sol_subscriber.subscribe(program_pubkey);

        // Subscribe to TON events
        self.context
            .ton_subscriber
            .add_transactions_subscription([*account], &observer);

        // Done
        Ok(())
    }

    fn add_btc_ton_event_configuration(
        &self,
        state: &mut BridgeState,
        account: &UInt256,
        contract: &ExistingContract,
    ) -> Result<()> {
        // Get configuration details
        let details = BtcTonEventConfigurationContract(contract)
            .get_details()
            .context("Failed to get BTC->TON event configuration details")?;

        // Add unique event hash
        add_event_code_hash(
            &mut state.event_code_hashes,
            &details.basic_configuration.event_code,
            EventType::BtcTon,
        )?;

        // Add configuration entry
        let observer = AccountObserver::new(&self.btc_ton_event_configurations_tx);
        match state.btc_ton_event_configurations.entry(*account) {
            hash_map::Entry::Vacant(entry) => {
                tracing::info!(
                    configuration = %DisplayAddr(account),
                    ?details, "added new BTC->TON event configuration"
                );

                self.total_active_btc_ton_event_configurations
                    .fetch_add(1, Ordering::Release);

                entry.insert(BtcTonEventConfigurationState {
                    details,
                    _observer: observer.clone(),
                });
            }
            hash_map::Entry::Occupied(_) => {
                tracing::info!(
                    configuration = %DisplayAddr(account),
                    "BTC->TON event configuration already exists",
                );
                return Err(BridgeError::EventConfigurationAlreadyExists.into());
            }
        };

        // Subscribe to TON events
        self.context
            .ton_subscriber
            .add_transactions_subscription([*account], &observer);

        // Done
        Ok(())
    }

    fn add_ton_btc_event_configuration(
        &self,
        state: &mut BridgeState,
        account: &UInt256,
        contract: &ExistingContract,
    ) -> Result<()> {
        // Get configuration details
        let details = TonBtcEventConfigurationContract(contract)
            .get_details()
            .context("Failed to get TON->BTC event configuration details")?;

        // Check if configuration is expired
        let current_timestamp = self.context.ton_subscriber.current_utime();
        if details.is_expired(current_timestamp) {
            // Do nothing in that case
            tracing::warn!(
                configuration = %DisplayAddr(account),
                current_timestamp,
                end_timestamp = details.network_configuration.end_timestamp,
                "ignoring disabled TON->BTC event configuration",
            );
            return Ok(());
        };

        // Add unique event hash
        add_event_code_hash(
            &mut state.event_code_hashes,
            &details.basic_configuration.event_code,
            EventType::TonBtc,
        )?;

        // Add configuration entry
        let observer = AccountObserver::new(&self.ton_btc_event_configurations_tx);
        match state.ton_btc_event_configurations.entry(*account) {
            hash_map::Entry::Vacant(entry) => {
                tracing::info!(
                    configuration = %DisplayAddr(account),
                    ?details,
                    "added new TON->BTC event configuration"
                );

                self.total_active_ton_btc_event_configurations
                    .fetch_add(1, Ordering::Release);

                entry.insert(TonBtcEventConfigurationState {
                    details,
                    _observer: observer.clone(),
                });
            }
            hash_map::Entry::Occupied(_) => {
                tracing::info!(
                    configuration = %DisplayAddr(account),
                    "TON->BTC event configuration already exists",
                );
                return Err(BridgeError::EventConfigurationAlreadyExists.into());
            }
        };

        // Subscribe to TON events
        self.context
            .ton_subscriber
            .add_transactions_subscription([*account], &observer);

        // Done
        Ok(())
    }

    async fn get_all_events(self: &Arc<Self>) -> Result<()> {
        type AccountsSet = FxHashSet<UInt256>;

        #[allow(clippy::too_many_arguments)]
        fn iterate_events(
            bridge: Arc<Bridge>,
            accounts: ton_block::ShardAccounts,
            event_code_hashes: Arc<EventCodeHashesMap>,
            unique_eth_ton_event_configurations: Arc<AccountsSet>,
            unique_ton_eth_event_configurations: Arc<AccountsSet>,
            unique_sol_ton_event_configurations: Arc<AccountsSet>,
            unique_ton_sol_event_configurations: Arc<AccountsSet>,
            unique_btc_ton_event_configurations: Arc<AccountsSet>,
            unique_ton_btc_event_configurations: Arc<AccountsSet>,
        ) -> Result<bool> {
            let our_public_key = bridge.context.keystore.ton.public_key();
            let has_sol_subscriber = bridge.context.sol_subscriber.is_some();
            let has_btc_subscriber = bridge.context.btc_subscriber.is_some();

            accounts.iterate_with_keys(|hash, shard_account| {
                // Prefetch only contract code hash
                let code_hash = match read_code_hash(&mut shard_account.account_cell().into())? {
                    Some(code_hash) => code_hash,
                    None => return Ok(true),
                };

                // Filter only known event contracts
                let event_type = match event_code_hashes.get(&code_hash) {
                    Some(event_type) => event_type,
                    None => return Ok(true),
                };

                // Read account from shard state
                let account = match shard_account.read_account()? {
                    ton_block::Account::Account(account) => account,
                    ton_block::Account::AccountNone => return Ok(true),
                };

                tracing::debug!(
                    event = %DisplayAddr(hash),
                    ?event_type,
                    "found event in shard state"
                );

                // Extract data
                let contract = ExistingContract {
                    account,
                    last_transaction_id: LastTransactionId::Exact(TransactionId {
                        lt: shard_account.last_trans_lt(),
                        hash: *shard_account.last_trans_hash(),
                    }),
                };

                macro_rules! check_configuration {
                    ($contract: ident) => {
                        match $contract(&contract).event_init_data() {
                            Ok(init_data) => init_data.configuration,
                            Err(e) => {
                                tracing::info!(
                                    event = %DisplayAddr(hash),
                                    ?event_type,
                                    "failed to get event init data: {e:?}"
                                );
                                return Ok(true);
                            }
                        }
                    };
                }

                // Process event
                match EventBaseContract(&contract).process(
                    our_public_key,
                    *event_type == EventType::TonEth,
                    *event_type == EventType::BtcTon || *event_type == EventType::TonBtc,
                ) {
                    Ok(EventAction::Nop | EventAction::Vote) => match event_type {
                        EventType::EthTon => {
                            let configuration = check_configuration!(EthTonEventContract);

                            if !unique_eth_ton_event_configurations.contains(&configuration) {
                                tracing::warn!(
                                    event = %DisplayAddr(hash),
                                    configuration = %DisplayAddr(configuration),
                                    "ETH->TON event configuration not found"
                                );
                                return Ok(true);
                            }

                            if bridge.add_pending_event(hash, &bridge.eth_ton_events_state) {
                                bridge.spawn_background_task(
                                    "initial update ETH->TON event",
                                    bridge.clone().update_eth_ton_event(hash),
                                );
                            }
                        }
                        EventType::TonEth => {
                            let configuration = check_configuration!(TonEthEventContract);

                            if !unique_ton_eth_event_configurations.contains(&configuration) {
                                tracing::warn!(
                                    event = %DisplayAddr(hash),
                                    configuration = %DisplayAddr(configuration),
                                    "TON->ETH event configuration not found",
                                );
                                return Ok(true);
                            }

                            if bridge.add_pending_event(hash, &bridge.ton_eth_events_state) {
                                bridge.spawn_background_task(
                                    "initial update TON->ETH event",
                                    bridge.clone().update_ton_eth_event(hash),
                                );
                            }
                        }
                        EventType::SolTon if has_sol_subscriber => {
                            let configuration = check_configuration!(SolTonEventContract);

                            if !unique_sol_ton_event_configurations.contains(&configuration) {
                                tracing::warn!(
                                    event = %DisplayAddr(hash),
                                    configuration = %DisplayAddr(configuration),
                                    "SOL->TON event configuration not found",
                                );
                                return Ok(true);
                            }

                            if bridge.add_pending_event(hash, &bridge.sol_ton_events_state) {
                                bridge.spawn_background_task(
                                    "initial update SOL->TON event",
                                    bridge.clone().update_sol_ton_event(hash),
                                );
                            }
                        }
                        EventType::TonSol if has_sol_subscriber => {
                            let configuration = check_configuration!(TonSolEventContract);

                            if !unique_ton_sol_event_configurations.contains(&configuration) {
                                tracing::warn!(
                                    event = %DisplayAddr(hash),
                                    configuration = %DisplayAddr(configuration),
                                    "TON->SOL event configuration not found",
                                );
                                return Ok(true);
                            }

                            if bridge.add_pending_event(hash, &bridge.ton_sol_events_state) {
                                bridge.spawn_background_task(
                                    "initial update TON->SOL event",
                                    bridge.clone().update_ton_sol_event(hash),
                                );
                            }
                        }
                        EventType::BtcTon if has_btc_subscriber => {
                            let configuration = check_configuration!(BtcTonEventContract);

                            if !unique_btc_ton_event_configurations.contains(&configuration) {
                                tracing::warn!(
                                    event = %DisplayAddr(hash),
                                    configuration = %DisplayAddr(configuration),
                                    "BTC->TON event configuration not found"
                                );
                                return Ok(true);
                            }

                            if bridge.add_pending_event(hash, &bridge.btc_ton_events_state) {
                                bridge.spawn_background_task(
                                    "initial update BTC->TON event",
                                    bridge.clone().update_btc_ton_event(hash),
                                );
                            }
                        }
                        EventType::TonBtc if has_btc_subscriber => {
                            let configuration = check_configuration!(TonBtcEventContract);

                            if !unique_ton_btc_event_configurations.contains(&configuration) {
                                tracing::warn!(
                                    event = %DisplayAddr(hash),
                                    configuration = %DisplayAddr(configuration),
                                    "TON->BTC event configuration not found"
                                );
                                return Ok(true);
                            }

                            if bridge.add_pending_event(hash, &bridge.ton_btc_events_state) {
                                bridge.spawn_background_task(
                                    "initial update BTC->TON event",
                                    bridge.clone().update_ton_btc_event(hash),
                                );
                            }
                        }
                        _ => {}
                    },
                    Ok(EventAction::Commit) => match event_type {
                        EventType::BtcTon => {
                            let configuration = check_configuration!(BtcTonEventContract);

                            if !unique_btc_ton_event_configurations.contains(&configuration) {
                                tracing::warn!(
                                    event = %DisplayAddr(hash),
                                    configuration = %DisplayAddr(configuration),
                                    "BTC->TON event configuration not found"
                                );
                                return Ok(true);
                            }

                            if bridge.add_pending_event(hash, &bridge.btc_ton_events_state) {
                                bridge.spawn_background_task(
                                    "initial commit BTC->TON event",
                                    bridge.clone().commit_btc_ton_event(hash),
                                );
                            }
                        }
                        EventType::TonBtc => {
                            let configuration = check_configuration!(TonBtcEventContract);

                            if !unique_ton_btc_event_configurations.contains(&configuration) {
                                tracing::warn!(
                                    event = %DisplayAddr(hash),
                                    configuration = %DisplayAddr(configuration),
                                    "TON->BTC event configuration not found"
                                );
                                return Ok(true);
                            }

                            if bridge.add_pending_event(hash, &bridge.btc_ton_events_state) {
                                bridge.spawn_background_task(
                                    "initial commit TON->BTC event",
                                    bridge.clone().commit_ton_btc_event(hash),
                                );
                            }
                        }
                        _ => {}
                    },
                    Ok(EventAction::Remove) => { /* do nothing */ }
                    Err(e) => {
                        tracing::error!(
                            event = %DisplayAddr(hash),
                            ?event_type,
                            "failed to get event details: {e:?}",
                        );
                    }
                }

                Ok(true)
            })
        }

        // Wait all accounts
        let shard_accounts = self.context.get_all_shard_accounts().await?;

        let start = std::time::Instant::now();

        // Lock state to prevent adding new configurations
        let state = self.state.read().await;

        // Prepare shard task context
        let event_code_hashes = Arc::new(state.event_code_hashes.clone());

        // NOTE: configuration sets are explicitly constructed from state instead of
        // just using [eth/ton]_event_counters. It is done on purpose to use the actual
        // configurations. It is acceptable that event counters will not be relevant
        let unique_eth_ton_event_configurations =
            Arc::new(state.unique_eth_ton_event_configurations());
        let unique_ton_eth_event_configurations =
            Arc::new(state.unique_ton_eth_event_configurations());
        let unique_sol_ton_event_configurations =
            Arc::new(state.unique_sol_ton_event_configurations());
        let unique_ton_sol_event_configurations =
            Arc::new(state.unique_ton_sol_event_configurations());
        let unique_btc_ton_event_configurations =
            Arc::new(state.unique_btc_ton_event_configurations());
        let unique_ton_btc_event_configurations =
            Arc::new(state.unique_ton_btc_event_configurations());

        // Process shards in parallel
        tracing::info!("started searching for all events");
        let mut results_rx = {
            let (results_tx, results_rx) = mpsc::unbounded_channel();

            for (shard_ident, accounts) in shard_accounts {
                let bridge = self.clone();
                let event_code_hashes = event_code_hashes.clone();
                let unique_eth_ton_event_configurations =
                    unique_eth_ton_event_configurations.clone();
                let unique_ton_eth_event_configurations =
                    unique_ton_eth_event_configurations.clone();
                let unique_sol_ton_event_configurations =
                    unique_sol_ton_event_configurations.clone();
                let unique_ton_sol_event_configurations =
                    unique_ton_sol_event_configurations.clone();
                let unique_btc_ton_event_configurations =
                    unique_btc_ton_event_configurations.clone();
                let unique_ton_btc_event_configurations =
                    unique_ton_btc_event_configurations.clone();
                let results_tx = results_tx.clone();

                tokio::spawn(tokio::task::spawn_blocking(move || {
                    let start = std::time::Instant::now();
                    let result = iterate_events(
                        bridge,
                        accounts,
                        event_code_hashes,
                        unique_eth_ton_event_configurations,
                        unique_ton_eth_event_configurations,
                        unique_sol_ton_event_configurations,
                        unique_ton_sol_event_configurations,
                        unique_btc_ton_event_configurations,
                        unique_ton_btc_event_configurations,
                    );
                    tracing::info!(
                        shard = shard_ident.shard_prefix_as_str_with_tag(),
                        elapsed_sec = start.elapsed().as_secs(),
                        "processed accounts in shard",
                    );
                    results_tx.send(result).ok();
                }));
            }

            results_rx
        };

        // Wait until all shards are processed
        while let Some(result) = results_rx.recv().await {
            if let Err(e) = result {
                return Err(e).context("Failed to find all events");
            }
        }

        // Done
        tracing::info!(
            elapsed_sec = start.elapsed().as_secs(),
            "finished iterating all events",
        );
        Ok(())
    }

    fn start_event_configurations_gc(self: &Arc<Self>) {
        let bridge = Arc::downgrade(self);

        tokio::spawn(async move {
            'outer: loop {
                tokio::time::sleep(Duration::from_secs(10)).await;

                // Get bridge if it is still alive
                let bridge = match bridge.upgrade() {
                    Some(bridge) => bridge,
                    None => return,
                };
                let ton_subscriber = &bridge.context.ton_subscriber;
                let ton_engine = &bridge.context.ton_engine;

                // Get current time from masterchain
                let current_utime = ton_subscriber.current_utime();

                // Check expired configurations
                let has_expired_ton_eth_configurations = {
                    let state = bridge.state.read().await;
                    state.has_expired_ton_eth_event_configurations(current_utime)
                };

                let has_expired_ton_sol_configurations = {
                    let state = bridge.state.read().await;
                    state.has_expired_ton_sol_event_configurations(current_utime)
                };

                let has_expired_sol_ton_configurations = {
                    let state = bridge.state.read().await;
                    state.has_expired_sol_ton_event_configurations(current_utime)
                };

                // Check expired configurations
                let has_expired_ton_btc_configurations = {
                    let state = bridge.state.read().await;
                    state.has_expired_ton_btc_event_configurations(current_utime)
                };

                // Do nothing if there are not expired configurations
                if !has_expired_ton_eth_configurations
                    && !has_expired_ton_sol_configurations
                    && !has_expired_sol_ton_configurations
                    && !has_expired_ton_btc_configurations
                {
                    continue;
                }

                // Wait all shards
                let current_utime = match ton_subscriber.wait_shards(None).await {
                    Ok(shards) => {
                        for (_, block_id) in shards.block_ids {
                            if let Err(e) = ton_engine.wait_state(&block_id, None, false).await {
                                tracing::error!(%block_id, "failed to wait for shard state: {e:?}");
                                continue 'outer;
                            }
                        }
                        shards.current_utime
                    }
                    Err(e) => {
                        tracing::error!("failed to wait for current shards info: {e:?}");
                        continue;
                    }
                };

                let mut state = bridge.state.write().await;

                // Remove TON->ETH expired configurations
                let mut total_removed = 0;
                state.ton_eth_event_configurations.retain(|account, state| {
                    if state.details.is_expired(current_utime) {
                        tracing::warn!(
                            configuration = %DisplayAddr(account),
                            current_utime,
                            "removing TON->ETH event configuration",
                        );
                        total_removed += 1;
                        false
                    } else {
                        true
                    }
                });
                bridge
                    .total_active_ton_eth_event_configurations
                    .fetch_sub(total_removed, Ordering::Release);

                // Remove TON->SOL expired configurations
                let mut total_removed = 0;
                state.ton_sol_event_configurations.retain(|account, state| {
                    if state.details.is_expired(current_utime) {
                        tracing::warn!(
                            configuration = %DisplayAddr(account),
                            current_utime,
                            "removing TON->SOL event configuration",
                        );
                        total_removed += 1;
                        false
                    } else {
                        true
                    }
                });
                bridge
                    .total_active_ton_sol_event_configurations
                    .fetch_sub(total_removed, Ordering::Release);

                // Remove SOL->TON expired configurations
                let mut total_removed = 0;
                state.sol_ton_event_configurations.retain(|account, state| {
                    if state.details.is_expired(current_utime as u64) {
                        tracing::warn!(
                            configuration = %DisplayAddr(account),
                            current_utime,
                            "removing SOL->TON event configuration",
                        );
                        total_removed += 1;
                        false
                    } else {
                        true
                    }
                });
                bridge
                    .total_active_sol_ton_event_configurations
                    .fetch_sub(total_removed, Ordering::Release);

                // Remove TON->BTC expired configurations
                let mut total_removed = 0;
                state.ton_btc_event_configurations.retain(|account, state| {
                    if state.details.is_expired(current_utime) {
                        tracing::warn!(
                            configuration = %DisplayAddr(account),
                            current_utime,
                            "removing TON->BTC event configuration",
                        );
                        total_removed += 1;
                        false
                    } else {
                        true
                    }
                });
                bridge
                    .total_active_ton_btc_event_configurations
                    .fetch_sub(total_removed, Ordering::Release);
            }
        });
    }

    /// Creates event observer if it doesn't exist and subscribes it to transactions
    fn add_pending_event<T>(&self, account: UInt256, state: &EventsState<T>) -> bool
    where
        T: std::fmt::Debug + ReadFromTransaction + 'static,
    {
        use dashmap::mapref::entry::Entry;

        let new_event = if let Entry::Vacant(entry) = state.pending.entry(account) {
            let observer = AccountObserver::new(&state.events_tx);
            entry.insert(PendingEventState {
                processing_started: AtomicBool::new(false),
                observer: observer.clone(),
            });
            self.context
                .ton_subscriber
                .add_transactions_subscription([account], &observer);
            true
        } else {
            false
        };

        // Update metrics
        // NOTE: use separate flag to reduce events map lock duration
        if new_event {
            state.count.fetch_add(1, Ordering::Release);
        }

        new_event
    }

    /// Waits future in background. In case of error does nothing but logging
    fn spawn_background_task<F>(self: &Arc<Self>, name: &'static str, fut: F)
    where
        F: Future<Output = Result<()>> + Send + 'static,
    {
        tokio::spawn(async move {
            if let Err(e) = fut.await {
                tracing::error!("failed to {name}: {e:?}");
            }
        });
    }
}

pub struct BridgeMetrics {
    pub pending_eth_ton_event_count: usize,
    pub pending_ton_eth_event_count: usize,
    pub pending_sol_ton_event_count: usize,
    pub pending_ton_sol_event_count: usize,
    pub pending_btc_ton_event_count: usize,
    pub pending_ton_btc_event_count: usize,
    pub total_active_eth_ton_event_configurations: usize,
    pub total_active_ton_eth_event_configurations: usize,
    pub total_active_sol_ton_event_configurations: usize,
    pub total_active_ton_sol_event_configurations: usize,
    pub total_active_btc_ton_event_configurations: usize,
    pub total_active_ton_btc_event_configurations: usize,
}

struct EventsState<T> {
    pending: FxDashMap<UInt256, PendingEventState<T>>,
    count: AtomicUsize,
    events_tx: AccountEventsTx<(T, EventStatus)>,
}

impl<T> EventsState<T>
where
    T: EventExt,
{
    fn new(events_tx: AccountEventsTx<(T, EventStatus)>) -> Arc<Self> {
        Arc::new(Self {
            pending: Default::default(),
            count: Default::default(),
            events_tx,
        })
    }

    /// Returns false if event processing was already started or event didn't exist
    fn start_processing(&self, account: &UInt256) -> bool {
        match self.pending.get(account) {
            Some(entry) => !entry.processing_started.fetch_or(true, Ordering::AcqRel),
            None => false,
        }
    }

    fn remove(&self, account: &UInt256) {
        if self.pending.remove(account).is_some() {
            self.count.fetch_sub(1, Ordering::Release);
        }
    }
}

struct PendingEventState<T> {
    processing_started: AtomicBool,
    observer: Arc<AccountObserver<(T, EventStatus)>>,
}

#[async_trait::async_trait]
trait EventExt {
    const REQUIRE_ALL_SIGNATURES: bool;
    const REQUIRE_COMMIT: bool;

    async fn update_event(bridge: Arc<Bridge>, account: UInt256) -> Result<()>;
}

#[async_trait::async_trait]
impl EventExt for EthTonEvent {
    const REQUIRE_ALL_SIGNATURES: bool = false;
    const REQUIRE_COMMIT: bool = false;

    async fn update_event(bridge: Arc<Bridge>, account: UInt256) -> Result<()> {
        bridge.update_eth_ton_event(account).await
    }
}

#[async_trait::async_trait]
impl EventExt for TonEthEvent {
    const REQUIRE_ALL_SIGNATURES: bool = true;
    const REQUIRE_COMMIT: bool = false;

    async fn update_event(bridge: Arc<Bridge>, account: UInt256) -> Result<()> {
        bridge.update_ton_eth_event(account).await
    }
}

#[async_trait::async_trait]
impl EventExt for SolTonEvent {
    const REQUIRE_ALL_SIGNATURES: bool = false;
    const REQUIRE_COMMIT: bool = false;

    async fn update_event(bridge: Arc<Bridge>, account: UInt256) -> Result<()> {
        bridge.update_sol_ton_event(account).await
    }
}

#[async_trait::async_trait]
impl EventExt for TonSolEvent {
    const REQUIRE_ALL_SIGNATURES: bool = false;
    const REQUIRE_COMMIT: bool = false;

    async fn update_event(bridge: Arc<Bridge>, account: UInt256) -> Result<()> {
        bridge.update_ton_sol_event(account).await
    }
}

#[async_trait::async_trait]
impl EventExt for BtcTonEvent {
    const REQUIRE_ALL_SIGNATURES: bool = false;
    const REQUIRE_COMMIT: bool = true;

    async fn update_event(bridge: Arc<Bridge>, account: UInt256) -> Result<()> {
        bridge.update_btc_ton_event(account).await
    }
}

#[async_trait::async_trait]
impl EventExt for TonBtcEvent {
    const REQUIRE_ALL_SIGNATURES: bool = true;
    const REQUIRE_COMMIT: bool = true;

    async fn update_event(bridge: Arc<Bridge>, account: UInt256) -> Result<()> {
        bridge.update_ton_btc_event(account).await
    }
}

/// Semi-persistent bridge contracts collection
#[derive(Default)]
struct BridgeState {
    connectors: ConnectorsMap,
    eth_ton_event_configurations: EthTonEventConfigurationsMap,
    ton_eth_event_configurations: TonEthEventConfigurationsMap,
    sol_ton_event_configurations: SolTonEventConfigurationsMap,
    ton_sol_event_configurations: TonSolEventConfigurationsMap,
    btc_ton_event_configurations: BtcTonEventConfigurationsMap,
    ton_btc_event_configurations: TonBtcEventConfigurationsMap,

    /// Unique event contracts code hashes.
    ///
    /// NOTE: only built on startup and then updated on each new configuration.
    /// Elements are not removed because it is not needed (the situation when one
    /// contract code will be used for ETH and TON simultaneously)
    event_code_hashes: EventCodeHashesMap,
}

impl BridgeState {
    fn has_expired_ton_eth_event_configurations(&self, current_timestamp: u32) -> bool {
        self.ton_eth_event_configurations
            .iter()
            .any(|(_, state)| state.details.is_expired(current_timestamp))
    }

    fn has_expired_ton_sol_event_configurations(&self, current_timestamp: u32) -> bool {
        self.ton_sol_event_configurations
            .iter()
            .any(|(_, state)| state.details.is_expired(current_timestamp))
    }

    fn has_expired_sol_ton_event_configurations(&self, current_timestamp: u32) -> bool {
        self.sol_ton_event_configurations
            .iter()
            .any(|(_, state)| state.details.is_expired(current_timestamp as u64))
    }

    fn has_expired_ton_btc_event_configurations(&self, current_timestamp: u32) -> bool {
        self.ton_btc_event_configurations
            .iter()
            .any(|(_, state)| state.details.is_expired(current_timestamp))
    }

    fn unique_eth_ton_event_configurations(&self) -> FxHashSet<UInt256> {
        self.eth_ton_event_configurations.keys().copied().collect()
    }

    fn unique_ton_eth_event_configurations(&self) -> FxHashSet<UInt256> {
        self.ton_eth_event_configurations.keys().copied().collect()
    }

    fn unique_sol_ton_event_configurations(&self) -> FxHashSet<UInt256> {
        self.sol_ton_event_configurations.keys().copied().collect()
    }

    fn unique_ton_sol_event_configurations(&self) -> FxHashSet<UInt256> {
        self.ton_sol_event_configurations.keys().copied().collect()
    }

    fn unique_btc_ton_event_configurations(&self) -> FxHashSet<UInt256> {
        self.btc_ton_event_configurations.keys().copied().collect()
    }

    fn unique_ton_btc_event_configurations(&self) -> FxHashSet<UInt256> {
        self.ton_btc_event_configurations.keys().copied().collect()
    }
}

fn add_event_code_hash(
    event_code_hashes: &mut EventCodeHashesMap,
    code: &ton_types::Cell,
    event_type: EventType,
) -> Result<()> {
    match event_code_hashes.entry(code.repr_hash()) {
        // Just insert if it was not in the map
        hash_map::Entry::Vacant(entry) => {
            entry.insert(event_type);
        }
        // Do nothing if it was there with the same event type, otherwise return an error
        hash_map::Entry::Occupied(entry) => {
            if entry.get() != &event_type {
                return Err(BridgeError::InvalidEventConfiguration.into());
            }
        }
    };
    Ok(())
}

fn read_code_hash(cell: &mut ton_types::SliceData) -> Result<Option<UInt256>> {
    // 1. Read account
    if !cell.get_next_bit()? && (cell.remaining_bits() == 0 || cell.get_next_int(3)? != 1) {
        // Empty account
        return Ok(None);
    }

    // 2. Skip non-standard address
    if cell.get_next_int(2)? != 0b10 || cell.get_next_bit()? {
        return Ok(None);
    }
    cell.move_by(8 + 256)?;

    // 3. Skip storage info
    // 3.1. Skip storage used
    ton_block::StorageUsed::skip(cell)?;
    // 3.2. Skip last paid
    cell.move_by(32)?;
    // 3.3. Skip due payment
    if cell.get_next_bit()? {
        ton_block::Grams::skip(cell)?;
    }

    // 4. Skip storage
    // 4.1. Skip last transaction lt
    cell.move_by(64)?;
    // 4.2. Skip balance
    ton_block::CurrencyCollection::skip(cell)?;

    // 5. Skip account state
    if !cell.get_next_bit()? {
        return Ok(None);
    }
    // 5.1. Skip optional split depth (`ton_block::Number5`)
    if cell.get_next_bit()? {
        cell.move_by(5)?;
    }
    // 5.2. Skip optional ticktock (`ton_block::TickTock`)
    if cell.get_next_bit()? {
        cell.move_by(2)?;
    }
    // 5.3. Skip empty code
    if !cell.get_next_bit()? {
        return Ok(None);
    }

    // Read code hash
    let code = cell.checked_drain_reference()?;
    Ok(Some(code.repr_hash()))
}

impl EventBaseContract<'_> {
    /// Determine event action
    fn process(
        &self,
        public_key: &UInt256,
        require_all_signatures: bool,
        require_commit: bool,
    ) -> Result<EventAction> {
        const SUPPORTED_API_VERSION: u32 = 2;

        Ok(match self.status()? {
            // If it is still initializing - postpone processing until relay keys are received
            EventStatus::Initializing => EventAction::Nop,
            // The main status in which we can vote
            EventStatus::Pending
                if self.get_voters(EventVote::Empty)?.contains(public_key)
                    && self.get_api_version().unwrap_or_default() == SUPPORTED_API_VERSION =>
            {
                EventAction::Vote
            }
            // Special case for TON->ETH event which must collect as much signatures as possible
            EventStatus::Confirmed
                if require_all_signatures
                    && self.0.account.storage.balance.grams.0 >= MIN_EVENT_BALANCE
                    && self.get_voters(EventVote::Empty)?.contains(public_key)
                    && self.get_api_version().unwrap_or_default() == SUPPORTED_API_VERSION =>
            {
                EventAction::Vote
            }
            // Special case for BTC->TON & TON-> BTC events to commit UTXO in cache when relay sync
            EventStatus::Confirmed if require_commit => EventAction::Commit,
            // Discard event in other cases
            _ => EventAction::Remove,
        })
    }
}

enum EventAction {
    /// Delay event processing
    Nop,
    /// Remove pending event
    Remove,
    /// Continue voting for event
    Vote,
    /// Commit event to cache
    Commit,
}

/// ETH->TON event configuration data
#[derive(Clone)]
struct EthTonEventConfigurationState {
    /// Configuration details
    details: EthTonEventConfigurationDetails,
    /// Parsed and mapped event ABI
    event_abi: Arc<EthEventAbi>,

    /// Observer must live as long as configuration lives
    _observer: Arc<AccountObserver<EthTonEventConfigurationEvent>>,
}

/// TON->ETH event configuration data
#[derive(Clone)]
struct TonEthEventConfigurationState {
    /// Configuration details
    details: TonEthEventConfigurationDetails,
    /// Parsed `eventData` ABI
    event_abi: Vec<ton_abi::Param>,

    /// Observer must live as long as configuration lives
    _observer: Arc<AccountObserver<TonEthEventConfigurationEvent>>,
}

impl TonEthEventConfigurationDetails {
    fn is_expired(&self, current_timestamp: u32) -> bool {
        (1..current_timestamp).contains(&self.network_configuration.end_timestamp)
    }
}

/// SOL->TON event configuration data
#[derive(Clone)]
struct SolTonEventConfigurationState {
    /// Configuration details
    details: SolTonEventConfigurationDetails,
    /// Parsed and mapped event ABI
    event_abi: Vec<ton_abi::Param>,

    /// Observer must live as long as configuration lives
    _observer: Arc<AccountObserver<SolTonEventConfigurationEvent>>,
}

impl SolTonEventConfigurationDetails {
    fn is_expired(&self, current_timestamp: u64) -> bool {
        (1..current_timestamp).contains(&self.network_configuration.end_timestamp)
    }
}

/// TON->SOL event configuration data
#[derive(Clone)]
struct TonSolEventConfigurationState {
    /// Configuration details
    details: TonSolEventConfigurationDetails,
    /// Parsed `eventData` ABI
    event_abi: Vec<ton_abi::Param>,

    /// Observer must live as long as configuration lives
    _observer: Arc<AccountObserver<TonSolEventConfigurationEvent>>,
}

impl TonSolEventConfigurationDetails {
    fn is_expired(&self, current_timestamp: u32) -> bool {
        (1..current_timestamp).contains(&self.network_configuration.end_timestamp)
    }
}

/// BTC->TON event configuration data
#[derive(Clone)]
struct BtcTonEventConfigurationState {
    /// Configuration details
    details: BtcTonEventConfigurationDetails,

    /// Observer must live as long as configuration lives
    _observer: Arc<AccountObserver<BtcTonEventConfigurationEvent>>,
}

/// TON->ETH event configuration data
#[derive(Clone)]
struct TonBtcEventConfigurationState {
    /// Configuration details
    details: TonBtcEventConfigurationDetails,

    /// Observer must live as long as configuration lives
    _observer: Arc<AccountObserver<TonBtcEventConfigurationEvent>>,
}

impl TonBtcEventConfigurationDetails {
    fn is_expired(&self, current_timestamp: u32) -> bool {
        (1..current_timestamp).contains(&self.network_configuration.end_timestamp)
    }
}

/// Parsed bridge event
#[derive(Debug, Clone)]
enum BridgeEvent {
    ConnectorDeployed(ConnectorDeployedEvent),
}

impl ReadFromTransaction for BridgeEvent {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        let mut event = None;
        ctx.iterate_events(|id, body| {
            let connector_deployed = bridge_contract::events::connector_deployed();
            if id == connector_deployed.id {
                match connector_deployed
                    .decode_input(body)
                    .and_then(|tokens| tokens.unpack().map_err(anyhow::Error::from))
                {
                    Ok(parsed) => event = Some(BridgeEvent::ConnectorDeployed(parsed)),
                    Err(e) => {
                        tracing::error!(
                            tx = ctx.transaction_hash.to_hex_string(),
                            "failed to parse bridge event: {e:?}"
                        );
                    }
                }
            }
        });
        event
    }
}

/// Parsed connector event
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum ConnectorEvent {
    Enable,
}

impl ReadFromTransaction for ConnectorEvent {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        let mut event = None;
        ctx.iterate_events(|id, _| {
            if id == connector_contract::events::enabled().id {
                event = Some(ConnectorEvent::Enable);
            }
        });
        event
    }
}

impl TxContext<'_> {
    fn find_new_event_contract_addresses(&self) -> Vec<UInt256> {
        let event = base_event_configuration_contract::events::new_event_contract();

        let mut result: Vec<UInt256> = Vec::new();
        self.iterate_events(|id, body| {
            if id == event.id {
                match event.decode_input(body).and_then(|tokens| {
                    tokens
                        .unpack_first::<ton_block::MsgAddressInt>()
                        .map_err(anyhow::Error::from)
                }) {
                    Ok(parsed) => result.push(only_account_hash(parsed)),
                    Err(e) => {
                        tracing::error!(
                            tx = self.transaction_hash.to_hex_string(),
                            "failed to parse NewEventContract event: {e:?}",
                        );
                    }
                }
            }
        });

        result
    }

    fn find_withdrawals(&self) -> Vec<BtcWithdrawal> {
        let event = ton_btc_event_contract::events::add_withdrawal();

        let mut result: Vec<BtcWithdrawal> = Vec::new();
        self.iterate_events(|id, body| {
            if id == event.id {
                match event.decode_input(body).and_then(|tokens| {
                    tokens
                        .unpack_first::<BtcWithdrawal>()
                        .map_err(anyhow::Error::from)
                }) {
                    Ok(parsed) => result.push(parsed),
                    Err(e) => {
                        tracing::error!(
                            tx = self.transaction_hash.to_hex_string(),
                            "failed to parse NewEventContract event: {e:?}",
                        );
                    }
                }
            }
        });

        result
    }
}

#[derive(Debug, Clone)]
enum TonEthEventConfigurationEvent {
    EventDeployed { address: UInt256 },
    SetEndTimestamp { end_timestamp: u32 },
}

impl ReadFromTransaction for TonEthEventConfigurationEvent {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        let in_msg_body = ctx.in_msg_internal()?.body()?;

        let set_end_timestamp = ton_eth_event_configuration_contract::set_end_timestamp();

        match read_function_id(&in_msg_body).ok()? {
            id if id == set_end_timestamp.input_id => {
                let end_timestamp = set_end_timestamp
                    .decode_input(in_msg_body, true)
                    .and_then(|tokens| tokens.unpack_first().map_err(anyhow::Error::from))
                    .ok()?;

                Some(Self::SetEndTimestamp { end_timestamp })
            }
            _ => {
                let events = ctx.find_new_event_contract_addresses();
                Some(Self::EventDeployed {
                    address: events.into_iter().next()?,
                })
            }
        }
    }
}

#[derive(Debug, Clone)]
enum EthTonEventConfigurationEvent {
    EventsDeployed { events: Vec<UInt256> },
    SetEndBlockNumber { end_block_number: u32 },
}

impl ReadFromTransaction for EthTonEventConfigurationEvent {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        let in_msg_body = ctx.in_msg_internal()?.body()?;

        let set_end_block_number = eth_ton_event_configuration_contract::set_end_block_number();

        match read_function_id(&in_msg_body).ok()? {
            id if id == set_end_block_number.input_id => {
                let end_block_number = set_end_block_number
                    .decode_input(in_msg_body, true)
                    .and_then(|tokens| tokens.unpack_first().map_err(anyhow::Error::from))
                    .ok()?;

                Some(Self::SetEndBlockNumber { end_block_number })
            }
            _ => {
                let events = ctx.find_new_event_contract_addresses();
                if events.is_empty() {
                    return None;
                }
                Some(Self::EventsDeployed { events })
            }
        }
    }
}

#[derive(Debug, Clone)]
enum TonSolEventConfigurationEvent {
    EventDeployed { address: UInt256 },
    SetEndTimestamp { end_timestamp: u32 },
}

impl ReadFromTransaction for TonSolEventConfigurationEvent {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        let in_msg_body = ctx.in_msg_internal()?.body()?;

        let set_end_timestamp = ton_sol_event_configuration_contract::set_end_timestamp();

        match nekoton_abi::read_function_id(&in_msg_body).ok()? {
            id if id == set_end_timestamp.input_id => {
                let end_timestamp = set_end_timestamp
                    .decode_input(in_msg_body, true)
                    .and_then(|tokens| tokens.unpack_first().map_err(anyhow::Error::from))
                    .ok()?;

                Some(Self::SetEndTimestamp { end_timestamp })
            }
            _ => {
                let events = ctx.find_new_event_contract_addresses();
                Some(Self::EventDeployed {
                    address: events.into_iter().next()?,
                })
            }
        }
    }
}

#[derive(Debug, Clone)]
enum SolTonEventConfigurationEvent {
    EventDeployed { address: UInt256 },
    SetEndTimestamp { end_timestamp: u64 },
}

impl ReadFromTransaction for SolTonEventConfigurationEvent {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        let in_msg_body = ctx.in_msg_internal()?.body()?;

        let set_end_timestamp = sol_ton_event_configuration_contract::set_end_timestamp();

        match nekoton_abi::read_function_id(&in_msg_body).ok()? {
            id if id == set_end_timestamp.input_id => {
                let end_timestamp = set_end_timestamp
                    .decode_input(in_msg_body, true)
                    .and_then(|tokens| tokens.unpack_first().map_err(anyhow::Error::from))
                    .ok()?;

                Some(Self::SetEndTimestamp { end_timestamp })
            }
            _ => {
                let events = ctx.find_new_event_contract_addresses();
                Some(Self::EventDeployed {
                    address: events.into_iter().next()?,
                })
            }
        }
    }
}

#[derive(Debug, Clone)]
enum BtcTonEventConfigurationEvent {
    EventsDeployed { events: Vec<UInt256> },
    SetEndBlockNumber { end_block_number: u32 },
}

impl ReadFromTransaction for BtcTonEventConfigurationEvent {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        let in_msg_body = ctx.in_msg_internal()?.body()?;
        let set_end_block_number = btc_ton_event_configuration_contract::set_end_block_number();

        match read_function_id(&in_msg_body).ok()? {
            id if id == set_end_block_number.input_id => {
                let end_block_number = set_end_block_number
                    .decode_input(in_msg_body, true)
                    .and_then(|tokens| tokens.unpack_first().map_err(anyhow::Error::from))
                    .ok()?;
                Some(Self::SetEndBlockNumber { end_block_number })
            }
            _ => {
                let events = ctx.find_new_event_contract_addresses();
                if events.is_empty() {
                    return None;
                }
                Some(Self::EventsDeployed { events })
            }
        }
    }
}

#[derive(Debug, Clone)]
enum TonBtcEventConfigurationEvent {
    EventDeployed { address: UInt256 },
    SetEndTimestamp { end_timestamp: u32 },
}

impl ReadFromTransaction for TonBtcEventConfigurationEvent {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        let in_msg_body = ctx.in_msg_internal()?.body()?;

        let set_end_timestamp = ton_btc_event_configuration_contract::set_end_timestamp();

        match read_function_id(&in_msg_body).ok()? {
            id if id == set_end_timestamp.input_id => {
                let end_timestamp = set_end_timestamp
                    .decode_input(in_msg_body, true)
                    .and_then(|tokens| tokens.unpack_first().map_err(anyhow::Error::from))
                    .ok()?;

                Some(Self::SetEndTimestamp { end_timestamp })
            }
            _ => {
                let events = ctx.find_new_event_contract_addresses();
                Some(Self::EventDeployed {
                    address: events.into_iter().next()?,
                })
            }
        }
    }
}

impl ReadFromTransaction for EventStatus {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        let state = ctx.get_account_state().ok()?;
        EventBaseContract(&state).status().ok()
    }
}

#[derive(Debug, Clone)]
enum EthTonEvent {
    ReceiveRoundRelays { keys: Vec<UInt256> },
    Confirm { public_key: UInt256 },
    Reject { public_key: UInt256 },
    Rejected,
}

impl ReadFromTransaction for EthTonEvent {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        if has_rejected_event(ctx) {
            return Some(Self::Rejected);
        }

        let in_msg = ctx.in_msg;
        match in_msg.header() {
            ton_block::CommonMsgInfo::ExtInMsgInfo(_) => {
                let (public_key, body) = read_external_in_msg(&in_msg.body()?)?;

                match read_function_id(&body) {
                    Ok(id) if id == eth_ton_event_contract::confirm().input_id => {
                        Some(EthTonEvent::Confirm { public_key })
                    }
                    Ok(id) if id == eth_ton_event_contract::reject().input_id => {
                        Some(EthTonEvent::Reject { public_key })
                    }
                    _ => None,
                }
            }
            ton_block::CommonMsgInfo::IntMsgInfo(_) => {
                let body = in_msg.body()?;

                match read_function_id(&body) {
                    Ok(id) if id == base_event_contract::receive_round_relays().input_id => {
                        let RelayKeys { items } = base_event_contract::receive_round_relays()
                            .decode_input(body, true)
                            .and_then(|tokens| tokens.unpack().map_err(anyhow::Error::from))
                            .ok()?;

                        Some(EthTonEvent::ReceiveRoundRelays { keys: items })
                    }
                    _ => None,
                }
            }
            ton_block::CommonMsgInfo::ExtOutMsgInfo(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
enum TonEthEvent {
    ReceiveRoundRelays { keys: Vec<UInt256> },
    Confirm { public_key: UInt256 },
    Reject { public_key: UInt256 },
    Rejected,
    Closed,
}

impl ReadFromTransaction for TonEthEvent {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        if has_rejected_event(ctx) {
            return Some(Self::Rejected);
        }

        let in_msg = ctx.in_msg;
        let event = match in_msg.header() {
            ton_block::CommonMsgInfo::ExtInMsgInfo(_) => {
                let (public_key, body) = read_external_in_msg(&in_msg.body()?)?;

                match read_function_id(&body) {
                    Ok(id) if id == ton_eth_event_contract::confirm().input_id => {
                        Some(TonEthEvent::Confirm { public_key })
                    }
                    Ok(id) if id == ton_eth_event_contract::reject().input_id => {
                        Some(TonEthEvent::Reject { public_key })
                    }
                    _ => None,
                }
            }
            ton_block::CommonMsgInfo::IntMsgInfo(_) => {
                let body = in_msg.body()?;

                match read_function_id(&body) {
                    Ok(id) if id == base_event_contract::receive_round_relays().input_id => {
                        let RelayKeys { items } = base_event_contract::receive_round_relays()
                            .decode_input(body, true)
                            .and_then(|tokens| tokens.unpack().map_err(anyhow::Error::from))
                            .ok()?;

                        Some(TonEthEvent::ReceiveRoundRelays { keys: items })
                    }
                    _ => None,
                }
            }
            ton_block::CommonMsgInfo::ExtOutMsgInfo(_) => None,
        };

        if event.is_none() {
            let balance = ctx.get_account_state().ok()?.account.storage.balance.grams;
            if balance.0 < MIN_EVENT_BALANCE {
                return Some(Self::Closed);
            }
        }

        event
    }
}

#[derive(Debug, Clone)]
enum SolTonEvent {
    ReceiveRoundRelays { keys: Vec<UInt256> },
    Confirm { public_key: UInt256 },
    Reject { public_key: UInt256 },
    Rejected,
}

impl ReadFromTransaction for SolTonEvent {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        if has_rejected_event(ctx) {
            return Some(Self::Rejected);
        }

        let in_msg = ctx.in_msg;
        match in_msg.header() {
            ton_block::CommonMsgInfo::ExtInMsgInfo(_) => {
                let (public_key, body) = read_external_in_msg(&in_msg.body()?)?;

                match read_function_id(&body) {
                    Ok(id) if id == sol_ton_event_contract::confirm().input_id => {
                        Some(SolTonEvent::Confirm { public_key })
                    }
                    Ok(id) if id == sol_ton_event_contract::reject().input_id => {
                        Some(SolTonEvent::Reject { public_key })
                    }
                    _ => None,
                }
            }
            ton_block::CommonMsgInfo::IntMsgInfo(_) => {
                let body = in_msg.body()?;

                match read_function_id(&body) {
                    Ok(id) if id == base_event_contract::receive_round_relays().input_id => {
                        let RelayKeys { items } = base_event_contract::receive_round_relays()
                            .decode_input(body, true)
                            .and_then(|tokens| tokens.unpack().map_err(anyhow::Error::from))
                            .ok()?;

                        Some(SolTonEvent::ReceiveRoundRelays { keys: items })
                    }
                    _ => None,
                }
            }
            ton_block::CommonMsgInfo::ExtOutMsgInfo(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
enum TonSolEvent {
    ReceiveRoundRelays { keys: Vec<UInt256> },
    Confirm { public_key: UInt256 },
    Reject { public_key: UInt256 },
    Rejected,
    Closed,
}

impl ReadFromTransaction for TonSolEvent {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        if has_rejected_event(ctx) {
            return Some(Self::Rejected);
        }

        let in_msg = ctx.in_msg;
        let event = match in_msg.header() {
            ton_block::CommonMsgInfo::ExtInMsgInfo(_) => {
                let (public_key, body) = read_external_in_msg(&in_msg.body()?)?;

                match read_function_id(&body) {
                    Ok(id) if id == ton_sol_event_contract::confirm().input_id => {
                        Some(TonSolEvent::Confirm { public_key })
                    }
                    Ok(id) if id == ton_sol_event_contract::reject().input_id => {
                        Some(TonSolEvent::Reject { public_key })
                    }
                    _ => None,
                }
            }
            ton_block::CommonMsgInfo::IntMsgInfo(_) => {
                let body = in_msg.body()?;

                match read_function_id(&body) {
                    Ok(id) if id == base_event_contract::receive_round_relays().input_id => {
                        let RelayKeys { items } = base_event_contract::receive_round_relays()
                            .decode_input(body, true)
                            .and_then(|tokens| tokens.unpack().map_err(anyhow::Error::from))
                            .ok()?;

                        Some(TonSolEvent::ReceiveRoundRelays { keys: items })
                    }
                    _ => None,
                }
            }
            ton_block::CommonMsgInfo::ExtOutMsgInfo(_) => None,
        };

        if event.is_none() {
            let balance = ctx.get_account_state().ok()?.account.storage.balance.grams;
            if balance.0 < MIN_EVENT_BALANCE {
                return Some(Self::Closed);
            }
        }

        event
    }
}

#[derive(Debug, Clone)]
enum BtcTonEvent {
    ReceiveRoundRelays { keys: Vec<UInt256> },
    Confirm { public_key: UInt256 },
    Reject { public_key: UInt256 },
    Rejected,
}

impl ReadFromTransaction for BtcTonEvent {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        if has_rejected_event(ctx) {
            return Some(Self::Rejected);
        }

        let in_msg = ctx.in_msg;
        match in_msg.header() {
            ton_block::CommonMsgInfo::ExtInMsgInfo(_) => {
                let (public_key, body) = read_external_in_msg(&in_msg.body()?)?;

                match read_function_id(&body) {
                    Ok(id) if id == sol_ton_event_contract::confirm().input_id => {
                        Some(BtcTonEvent::Confirm { public_key })
                    }
                    Ok(id) if id == sol_ton_event_contract::reject().input_id => {
                        Some(BtcTonEvent::Reject { public_key })
                    }
                    _ => None,
                }
            }
            ton_block::CommonMsgInfo::IntMsgInfo(_) => {
                let body = in_msg.body()?;

                match read_function_id(&body) {
                    Ok(id) if id == base_event_contract::receive_round_relays().input_id => {
                        let RelayKeys { items } = base_event_contract::receive_round_relays()
                            .decode_input(body, true)
                            .and_then(|tokens| tokens.unpack().map_err(anyhow::Error::from))
                            .ok()?;

                        Some(BtcTonEvent::ReceiveRoundRelays { keys: items })
                    }
                    _ => None,
                }
            }
            ton_block::CommonMsgInfo::ExtOutMsgInfo(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
enum TonBtcEvent {
    ReceiveRoundRelays { keys: Vec<UInt256> },
    Confirm { public_key: UInt256 },
    Reject { public_key: UInt256 },
    Cancel { public_key: UInt256 },
    Withdrawal { withdrawal: BtcWithdrawal },
    Rejected,
    Closed,
}

impl ReadFromTransaction for TonBtcEvent {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        if has_rejected_event(ctx) {
            return Some(Self::Rejected);
        }

        let in_msg = ctx.in_msg;
        let event = match in_msg.header() {
            ton_block::CommonMsgInfo::ExtInMsgInfo(_) => {
                let (public_key, body) = read_external_in_msg(&in_msg.body()?)?;

                match read_function_id(&body) {
                    Ok(id) if id == ton_btc_event_contract::confirm().input_id => {
                        Some(TonBtcEvent::Confirm { public_key })
                    }
                    Ok(id) if id == ton_btc_event_contract::reject().input_id => {
                        Some(TonBtcEvent::Reject { public_key })
                    }
                    Ok(id) if id == ton_btc_event_contract::cancel().input_id => {
                        Some(TonBtcEvent::Cancel { public_key })
                    }
                    _ => None,
                }
            }
            ton_block::CommonMsgInfo::IntMsgInfo(_) => {
                let body = in_msg.body()?;

                match read_function_id(&body) {
                    Ok(id) if id == base_event_contract::receive_round_relays().input_id => {
                        let RelayKeys { items } = base_event_contract::receive_round_relays()
                            .decode_input(body, true)
                            .and_then(|tokens| tokens.unpack().map_err(anyhow::Error::from))
                            .ok()?;

                        Some(TonBtcEvent::ReceiveRoundRelays { keys: items })
                    }
                    _ => {
                        let withdrawals = ctx.find_withdrawals();
                        Some(Self::Withdrawal {
                            withdrawal: withdrawals.into_iter().next()?,
                        })
                    }
                }
            }
            ton_block::CommonMsgInfo::ExtOutMsgInfo(_) => None,
        };

        if event.is_none() {
            let balance = ctx.get_account_state().ok()?.account.storage.balance.grams;
            if balance.0 < MIN_EVENT_BALANCE {
                return Some(Self::Closed);
            }
        }

        event
    }
}
fn read_external_in_msg(body: &ton_types::SliceData) -> Option<(UInt256, ton_types::SliceData)> {
    match unpack_headers::<DefaultHeaders>(body) {
        Ok(((Some(public_key), _, _), body)) => Some((public_key, body)),
        _ => None,
    }
}

fn has_rejected_event(ctx: &TxContext<'_>) -> bool {
    let mut result = false;
    ctx.iterate_events(|event_id, _| {
        if event_id == base_event_contract::events::rejected().id {
            result = true;
        }
    });
    result
}

fn parse_client_error(err: ClientError) -> anyhow::Error {
    match &err.kind {
        ClientErrorKind::RpcError(RpcError::RpcResponseError {
            data:
                RpcResponseErrorData::SendTransactionPreflightFailure(RpcSimulateTransactionResult {
                    err: Some(TransactionError::InstructionError(_, InstructionError::Custom(code))),
                    ..
                }),
            ..
        }) => {
            let error = SolanaBridgeError::try_from(*code).trust_me();
            match error {
                SolanaBridgeError::EmergencyEnabled => {
                    anyhow::Error::msg(SolanaBridgeError::EmergencyEnabled.to_string())
                }
                SolanaBridgeError::VotesOverflow => {
                    anyhow::Error::msg(SolanaBridgeError::VotesOverflow.to_string())
                }
                SolanaBridgeError::InvalidVote => {
                    anyhow::Error::msg(SolanaBridgeError::InvalidVote.to_string())
                }
                SolanaBridgeError::InvalidRelay => {
                    anyhow::Error::msg(SolanaBridgeError::InvalidRelay.to_string())
                }
                _ => anyhow::Error::msg(format!("Solana RPC error: {err}")),
            }
        }
        ClientErrorKind::TransactionError(TransactionError::InstructionError(
            _,
            InstructionError::Custom(code),
        )) => {
            let error = SolanaBridgeError::try_from(*code).trust_me();
            match error {
                SolanaBridgeError::EmergencyEnabled => {
                    anyhow::Error::msg(SolanaBridgeError::EmergencyEnabled.to_string())
                }
                SolanaBridgeError::VotesOverflow => {
                    anyhow::Error::msg(SolanaBridgeError::VotesOverflow.to_string())
                }
                SolanaBridgeError::InvalidVote => {
                    anyhow::Error::msg(SolanaBridgeError::InvalidVote.to_string())
                }
                SolanaBridgeError::InvalidRelay => {
                    anyhow::Error::msg(SolanaBridgeError::InvalidRelay.to_string())
                }
                _ => anyhow::Error::msg(format!("Solana Transaction error: {err}")),
            }
        }
        _ => anyhow::Error::msg(format!("Solana Client error: {err}")),
    }
}

const MIN_EVENT_BALANCE: u128 = 100_000_000; // 0.1 TON

type ConnectorState = Arc<AccountObserver<ConnectorEvent>>;

type DefaultHeaders = (PubkeyHeader, TimeHeader, ExpireHeader);

type ConnectorsMap = FxHashMap<UInt256, ConnectorState>;
type EthTonEventConfigurationsMap = FxHashMap<UInt256, EthTonEventConfigurationState>;
type TonEthEventConfigurationsMap = FxHashMap<UInt256, TonEthEventConfigurationState>;
type SolTonEventConfigurationsMap = FxHashMap<UInt256, SolTonEventConfigurationState>;
type TonSolEventConfigurationsMap = FxHashMap<UInt256, TonSolEventConfigurationState>;
type BtcTonEventConfigurationsMap = FxHashMap<UInt256, BtcTonEventConfigurationState>;
type TonBtcEventConfigurationsMap = FxHashMap<UInt256, TonBtcEventConfigurationState>;
type EventCodeHashesMap = FxHashMap<UInt256, EventType>;

#[derive(Debug, Clone, Hash)]
pub enum VerificationStatus {
    Exists,
    NotExists { reason: String },
}

#[derive(thiserror::Error, Debug)]
enum BridgeError {
    #[error("Unknown chain id")]
    UnknownChainId,
    #[error("Unknown connector")]
    UnknownConnector,
    #[error("Unknown event configuration")]
    UnknownConfiguration,
    #[error("Bridge account not found")]
    BridgeAccountNotFound,
    #[error("Invalid event configuration")]
    InvalidEventConfiguration,
    #[error("Event configuration already exists")]
    EventConfigurationAlreadyExists,
}
