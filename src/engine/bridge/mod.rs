use std::collections::hash_map;
use std::future::Future;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use borsh::BorshDeserialize;
#[allow(unused_imports)]
use evm_tvm_abi_converter::{
    decode_ton_event_abi as decode_tvm_event_abi, make_mapped_ton_event as make_mapped_tvm_event,
    map_ton_tokens_to_eth_bytes as map_tvm_tokens_to_evm_bytes, EthEventAbi as EvmEventAbi,
    EthToTonMappingContext as EvmToTvmMappingContext, TonToEthContext as TvmToEvmContext,
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
use ton_types::UInt256;

use crate::engine::keystore::*;
use crate::engine::svm_subscriber::*;
use crate::engine::tvm_contracts::*;
use crate::engine::tvm_subscriber::*;
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

    // Observers for pending EVM->TVM events
    evm_tvm_events_state: Arc<EventsState<EvmTvmEvent>>,

    // Observers for pending TVM->EVM events
    tvm_evm_events_state: Arc<EventsState<TvmEvmEvent>>,

    // Observers for pending SVM->TVM events
    svm_tvm_events_state: Arc<EventsState<SvmTvmEvent>>,

    // Observers for pending TVM->SVM events
    tvm_svm_events_state: Arc<EventsState<TvmSvmEvent>>,

    connectors_tx: AccountEventsTx<ConnectorEvent>,
    evm_tvm_event_configurations_tx: AccountEventsTx<EvmTvmEventConfigurationEvent>,
    tvm_evm_event_configurations_tx: AccountEventsTx<TvmEvmEventConfigurationEvent>,
    svm_tvm_event_configurations_tx: AccountEventsTx<SvmTvmEventConfigurationEvent>,
    tvm_svm_event_configurations_tx: AccountEventsTx<TvmSvmEventConfigurationEvent>,

    total_active_evm_tvm_event_configurations: AtomicUsize,
    total_active_tvm_evm_event_configurations: AtomicUsize,
    total_active_svm_tvm_event_configurations: AtomicUsize,
    total_active_tvm_svm_event_configurations: AtomicUsize,
}

impl Bridge {
    pub async fn new(context: Arc<EngineContext>, bridge_account: UInt256) -> Result<Arc<Self>> {
        // Create bridge
        let (bridge_events_tx, bridge_events_rx) = mpsc::unbounded_channel();
        let (connectors_tx, connectors_rx) = mpsc::unbounded_channel();
        let (evm_tvm_event_configurations_tx, evm_tvm_event_configurations_rx) =
            mpsc::unbounded_channel();
        let (tvm_evm_event_configurations_tx, tvm_evm_event_configurations_rx) =
            mpsc::unbounded_channel();
        let (svm_tvm_event_configurations_tx, svm_tvm_event_configurations_rx) =
            mpsc::unbounded_channel();
        let (tvm_svm_event_configurations_tx, tvm_svm_event_configurations_rx) =
            mpsc::unbounded_channel();
        let (evm_tvm_events_tx, evm_tvm_events_rx) = mpsc::unbounded_channel();
        let (tvm_evm_events_tx, tvm_evm_events_rx) = mpsc::unbounded_channel();
        let (svm_tvm_events_tx, svm_tvm_events_rx) = mpsc::unbounded_channel();
        let (tvm_svm_events_tx, tvm_svm_events_rx) = mpsc::unbounded_channel();

        let bridge_observer = AccountObserver::new(&bridge_events_tx);

        let bridge = Arc::new(Bridge {
            context,
            bridge_account,
            bridge_observer: bridge_observer.clone(),
            state: Default::default(),
            evm_tvm_events_state: EventsState::new(evm_tvm_events_tx),
            tvm_evm_events_state: EventsState::new(tvm_evm_events_tx),
            svm_tvm_events_state: EventsState::new(svm_tvm_events_tx),
            tvm_svm_events_state: EventsState::new(tvm_svm_events_tx),
            connectors_tx,
            evm_tvm_event_configurations_tx,
            tvm_evm_event_configurations_tx,
            svm_tvm_event_configurations_tx,
            tvm_svm_event_configurations_tx,
            total_active_evm_tvm_event_configurations: Default::default(),
            total_active_tvm_evm_event_configurations: Default::default(),
            total_active_svm_tvm_event_configurations: Default::default(),
            total_active_tvm_svm_event_configurations: Default::default(),
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
            "EvmTvmEventConfigurationContract",
            evm_tvm_event_configurations_rx,
            Self::process_evm_tvm_event_configuration_event,
        );

        start_listening_events(
            &bridge,
            "TvmEvmEventConfigurationContract",
            tvm_evm_event_configurations_rx,
            Self::process_tvm_evm_event_configuration_event,
        );

        if bridge.context.svm_subscriber.is_some() {
            start_listening_events(
                &bridge,
                "SvmTvmEventConfigurationContract",
                svm_tvm_event_configurations_rx,
                Self::process_svm_tvm_event_configuration_event,
            );

            start_listening_events(
                &bridge,
                "TvmSvmEventConfigurationContract",
                tvm_svm_event_configurations_rx,
                Self::process_tvm_svm_event_configuration_event,
            );
        }

        start_listening_events(
            &bridge,
            "EvmTvmEventContract",
            evm_tvm_events_rx,
            Self::process_evm_tvm_event,
        );

        start_listening_events(
            &bridge,
            "TvmEvmEventContract",
            tvm_evm_events_rx,
            Self::process_tvm_evm_event,
        );

        if bridge.context.svm_subscriber.is_some() {
            start_listening_events(
                &bridge,
                "SvmTvmEventContract",
                svm_tvm_events_rx,
                Self::process_svm_tvm_event,
            );

            start_listening_events(
                &bridge,
                "TvmSvmEventContract",
                tvm_svm_events_rx,
                Self::process_tvm_svm_event,
            );
        }

        // Subscribe bridge account to transactions
        bridge
            .context
            .tvm_subscriber
            .add_transactions_subscription([bridge.bridge_account], &bridge.bridge_observer)
            .await;

        // Initialize
        bridge.get_all_configurations().await?;
        bridge.get_all_events().await?;

        bridge.start_event_configurations_gc();

        Ok(bridge)
    }

    pub fn metrics(&self) -> BridgeMetrics {
        BridgeMetrics {
            pending_evm_tvm_event_count: self.evm_tvm_events_state.count.load(Ordering::Acquire),
            pending_tvm_evm_event_count: self.tvm_evm_events_state.count.load(Ordering::Acquire),
            pending_svm_tvm_event_count: self.svm_tvm_events_state.count.load(Ordering::Acquire),
            pending_tvm_svm_event_count: self.tvm_svm_events_state.count.load(Ordering::Acquire),
            total_active_evm_tvm_event_configurations: self
                .total_active_evm_tvm_event_configurations
                .load(Ordering::Acquire),
            total_active_tvm_evm_event_configurations: self
                .total_active_tvm_evm_event_configurations
                .load(Ordering::Acquire),
            total_active_svm_tvm_event_configurations: self
                .total_active_svm_tvm_event_configurations
                .load(Ordering::Acquire),
            total_active_tvm_svm_event_configurations: self
                .total_active_tvm_svm_event_configurations
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
                            .tvm_subscriber
                            .add_transactions_subscription([event.connector], entry)
                            .await;
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

    async fn process_evm_tvm_event_configuration_event(
        self: Arc<Self>,
        (account, event): (UInt256, EvmTvmEventConfigurationEvent),
    ) -> Result<()> {
        match event {
            // Create observer on each deployment event
            EvmTvmEventConfigurationEvent::EventsDeployed { events } => {
                for address in events {
                    if self
                        .add_pending_event(address, &self.evm_tvm_events_state)
                        .await
                    {
                        let this = self.clone();
                        self.spawn_background_task("preprocess EVM->TVM event", async move {
                            this.preprocess_event(address, &this.evm_tvm_events_state)
                                .await
                        });
                    }
                }
            }
            // Update configuration state
            EvmTvmEventConfigurationEvent::SetEndBlockNumber { end_block_number } => {
                let mut state = self.state.write().await;
                let configuration = state
                    .evm_tvm_event_configurations
                    .get_mut(&account)
                    .ok_or(BridgeError::UnknownConfiguration)?;
                configuration.details.network_configuration.end_block_number = end_block_number;
            }
        }
        Ok(())
    }

    async fn process_tvm_evm_event_configuration_event(
        self: Arc<Self>,
        (account, event): (UInt256, TvmEvmEventConfigurationEvent),
    ) -> Result<()> {
        match event {
            // Create observer on each deployment event
            TvmEvmEventConfigurationEvent::EventDeployed { address, .. } => {
                if self
                    .add_pending_event(address, &self.tvm_evm_events_state)
                    .await
                {
                    let this = self.clone();
                    self.spawn_background_task("preprocess TVM->EVM event", async move {
                        this.preprocess_event(address, &this.tvm_evm_events_state)
                            .await
                    });
                } else {
                    // NOTE: Each TVM event must be unique on the contracts level,
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
            TvmEvmEventConfigurationEvent::SetEndTimestamp { end_timestamp } => {
                let mut state = self.state.write().await;
                let configuration = state
                    .tvm_evm_event_configurations
                    .get_mut(&account)
                    .ok_or(BridgeError::UnknownConfiguration)?;
                configuration.details.network_configuration.end_timestamp = end_timestamp;
            }
        }
        Ok(())
    }

    async fn process_svm_tvm_event_configuration_event(
        self: Arc<Self>,
        (account, event): (UInt256, SvmTvmEventConfigurationEvent),
    ) -> Result<()> {
        match event {
            // Create observer on each deployment event
            SvmTvmEventConfigurationEvent::EventsDeployed { events } => {
                for address in events {
                    if self
                        .add_pending_event(address, &self.svm_tvm_events_state)
                        .await
                    {
                        let this = self.clone();
                        self.spawn_background_task("preprocess SVM->TVM event", async move {
                            this.preprocess_event(address, &this.svm_tvm_events_state)
                                .await
                        });
                    }
                }
            }
            // Update configuration state
            SvmTvmEventConfigurationEvent::SetEndTimestamp { end_timestamp } => {
                let mut state = self.state.write().await;
                let configuration = state
                    .svm_tvm_event_configurations
                    .get_mut(&account)
                    .ok_or(BridgeError::UnknownConfiguration)?;
                configuration.details.network_configuration.end_timestamp = end_timestamp;
            }
        }
        Ok(())
    }

    async fn process_tvm_svm_event_configuration_event(
        self: Arc<Self>,
        (account, event): (UInt256, TvmSvmEventConfigurationEvent),
    ) -> Result<()> {
        match event {
            // Create observer on each deployment event
            TvmSvmEventConfigurationEvent::EventDeployed { address, .. } => {
                if self
                    .add_pending_event(address, &self.tvm_svm_events_state)
                    .await
                {
                    let this = self.clone();
                    self.spawn_background_task("preprocess TVM->SVM event", async move {
                        this.preprocess_event(address, &this.tvm_svm_events_state)
                            .await
                    });
                } else {
                    // NOTE: Each TVM event must be unique on the contracts level,
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
            TvmSvmEventConfigurationEvent::SetEndTimestamp { end_timestamp } => {
                let mut state = self.state.write().await;
                let configuration = state
                    .tvm_svm_event_configurations
                    .get_mut(&account)
                    .ok_or(BridgeError::UnknownConfiguration)?;
                configuration.details.network_configuration.end_timestamp = end_timestamp;
            }
        }
        Ok(())
    }

    async fn process_evm_tvm_event(
        self: Arc<Self>,
        (account, event): (UInt256, (EvmTvmEvent, EventStatus)),
    ) -> Result<()> {
        use dashmap::mapref::entry::Entry;

        let our_public_key = self.context.keystore.tvm.public_key();

        // Use flag to update counter outside events map lock to reduce its duration
        let mut event_removed = false;

        // Handle only known EVM events
        if let Entry::Occupied(entry) = self.evm_tvm_events_state.pending.entry(account) {
            let remove_entry = || {
                // Remove pending event
                entry.remove();
                event_removed = true;
            };

            match event {
                // Remove event if voting process was finished
                (EvmTvmEvent::Rejected, _)
                | (_, EventStatus::Confirmed | EventStatus::Rejected) => remove_entry(),
                // Handle event initialization
                (EvmTvmEvent::ReceiveRoundRelays { keys }, _) => {
                    // Check if event contains our key
                    if keys.contains(our_public_key) {
                        // Start voting
                        self.spawn_background_task(
                            "update EVM->TVM event",
                            self.clone().update_evm_tvm_event(account),
                        );
                    } else {
                        remove_entry();
                    }
                }
                // Handle our confirmation or rejection
                (EvmTvmEvent::Confirm { public_key } | EvmTvmEvent::Reject { public_key }, _)
                    if public_key == our_public_key =>
                {
                    remove_entry()
                }
                _ => { /* Ignore other events */ }
            }
        }

        // Update metrics
        if event_removed {
            self.evm_tvm_events_state
                .count
                .fetch_sub(1, Ordering::Release);
        }

        Ok(())
    }

    async fn process_tvm_evm_event(
        self: Arc<Self>,
        (account, event): (UInt256, (TvmEvmEvent, EventStatus)),
    ) -> Result<()> {
        use dashmap::mapref::entry::Entry;

        let our_public_key = self.context.keystore.tvm.public_key();

        // Use flag to update counter outside events map lock to reduce its duration
        let mut event_removed = false;

        // Handle only known TVM events
        if let Entry::Occupied(entry) = self.tvm_evm_events_state.pending.entry(account) {
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
                (TvmEvmEvent::Closed, EventStatus::Confirmed) => remove_entry(),
                // Remove event if it was rejected
                (TvmEvmEvent::Rejected, _) | (_, EventStatus::Rejected) => remove_entry(),
                // Handle event initialization
                (TvmEvmEvent::ReceiveRoundRelays { keys }, _) => {
                    // Check if event contains our key
                    if keys.contains(our_public_key) {
                        // Start voting
                        self.spawn_background_task(
                            "update TVM->EVM event",
                            self.clone().update_tvm_evm_event(account),
                        );
                    } else {
                        remove_entry();
                    }
                }
                // Handle our confirmation or rejection
                (TvmEvmEvent::Confirm { public_key } | TvmEvmEvent::Reject { public_key }, _)
                    if public_key == our_public_key =>
                {
                    remove_entry();
                }
                _ => { /* Ignore other events */ }
            }
        }

        // Update metrics
        if event_removed {
            self.tvm_evm_events_state
                .count
                .fetch_sub(1, Ordering::Release);
        }

        Ok(())
    }

    async fn process_svm_tvm_event(
        self: Arc<Self>,
        (account, event): (UInt256, (SvmTvmEvent, EventStatus)),
    ) -> Result<()> {
        use dashmap::mapref::entry::Entry;

        let our_public_key = self.context.keystore.svm.public_key_bytes();

        // Use flag to update counter outside events map lock to reduce its duration
        let mut event_removed = false;

        // Handle only known SVM events
        if let Entry::Occupied(entry) = self.svm_tvm_events_state.pending.entry(account) {
            let remove_entry = || {
                // Remove pending event
                entry.remove();
                event_removed = true;
            };

            match event {
                // Remove event if voting process was finished
                (SvmTvmEvent::Rejected, _)
                | (_, EventStatus::Confirmed | EventStatus::Rejected) => remove_entry(),
                // Handle event initialization
                (SvmTvmEvent::ReceiveRoundRelays { keys }, _) => {
                    // Check if event contains our key
                    if keys.contains(our_public_key) {
                        // Start voting
                        self.spawn_background_task(
                            "update SVM->TVM event",
                            self.clone().update_svm_tvm_event(account),
                        );
                    } else {
                        remove_entry();
                    }
                }
                // Handle our confirmation or rejection
                (SvmTvmEvent::Confirm { public_key } | SvmTvmEvent::Reject { public_key }, _)
                    if public_key == our_public_key =>
                {
                    remove_entry()
                }
                _ => { /* Ignore other events */ }
            }
        }

        // Update metrics
        if event_removed {
            self.svm_tvm_events_state
                .count
                .fetch_sub(1, Ordering::Release);
        }

        Ok(())
    }

    async fn process_tvm_svm_event(
        self: Arc<Self>,
        (account, event): (UInt256, (TvmSvmEvent, EventStatus)),
    ) -> Result<()> {
        use dashmap::mapref::entry::Entry;

        let our_public_key = self.context.keystore.tvm.public_key();

        // Use flag to update counter outside events map lock to reduce its duration
        let mut event_removed = false;

        // Handle only known TVM events
        if let Entry::Occupied(entry) = self.tvm_svm_events_state.pending.entry(account) {
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
                (TvmSvmEvent::Closed, EventStatus::Confirmed) => remove_entry(),
                // Remove event if it was rejected
                (TvmSvmEvent::Rejected, _) | (_, EventStatus::Rejected) => remove_entry(),

                // Handle event initialization
                (TvmSvmEvent::ReceiveRoundRelays { keys }, _) => {
                    // Check if event contains our key
                    if keys.contains(our_public_key) {
                        // Start voting
                        self.spawn_background_task(
                            "update TVM->SVM event",
                            self.clone().update_tvm_svm_event(account),
                        );
                    } else {
                        remove_entry();
                    }
                }
                // Handle our confirmation or rejection
                (TvmSvmEvent::Confirm { public_key } | TvmSvmEvent::Reject { public_key }, _)
                    if public_key == our_public_key =>
                {
                    remove_entry();
                }
                _ => { /* Ignore other events */ }
            }
        }

        // Update metrics
        if event_removed {
            self.tvm_svm_events_state
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
        let tvm_subscriber = &self.context.tvm_subscriber;
        let contract = tvm_subscriber.wait_contract_state(&account).await?;
        let base_event_contract = EventBaseContract(&contract);

        // Check further steps based on event statuses
        match base_event_contract.process(
            self.context.keystore.tvm.public_key(),
            T::REQUIRE_ALL_SIGNATURES,
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
            // NOTE: it is ok to update_tvm_event twice because in fact it will
            // do something only once
            EventAction::Vote => T::update_event(self.clone(), account).await,
        }
    }

    async fn update_evm_tvm_event(self: Arc<Self>, account: UInt256) -> Result<()> {
        if !self.evm_tvm_events_state.start_processing(&account) {
            return Ok(());
        }

        let keystore = &self.context.keystore;
        let tvm_subscriber = &self.context.tvm_subscriber;
        let evm_subscribers = &self.context.evm_subscribers;

        // Wait contract state
        let contract = tvm_subscriber.wait_contract_state(&account).await?;

        match EventBaseContract(&contract).process(keystore.tvm.public_key(), false)? {
            EventAction::Nop => return Ok(()),
            EventAction::Remove => {
                self.evm_tvm_events_state.remove(&account);
                return Ok(());
            }
            EventAction::Vote => { /* continue voting */ }
        }

        let event_init_data = EvmTvmEventContract(&contract).event_init_data()?;

        struct ConfigData {
            chain_id: u32,
            event_emitter: [u8; 20],
            abi: Arc<EvmEventAbi>,
            blocks_to_confirm: u16,
            check_token_root: bool,
        }

        // Get event configuration data
        let data = {
            let state = self.state.read().await;
            state
                .evm_tvm_event_configurations
                .get(&event_init_data.configuration)
                .map(|configuration| ConfigData {
                    chain_id: configuration.details.network_configuration.chain_id,
                    event_emitter: configuration.details.network_configuration.event_emitter,
                    abi: configuration.event_abi.clone(),
                    blocks_to_confirm: configuration
                        .details
                        .network_configuration
                        .event_blocks_to_confirm,
                    check_token_root: configuration.mapping_context.check_token_root,
                })
        };

        // NOTE: be sure to drop `evm_event_configurations` lock before that
        let (
            evm_subscriber,
            event_emitter,
            event_abi,
            blocks_to_confirm,
            preliminary_checks_succeeded,
        ) = match data {
            // Configuration found
            Some(ConfigData {
                chain_id,
                event_emitter,
                abi,
                blocks_to_confirm,
                check_token_root,
            }) => {
                let mut preliminary_checks_succeeded = true;
                // Check token root if required
                if check_token_root {
                    tracing::info!(
                        event = %DisplayAddr(account),
                        chain_id,
                        "EVM->TVM checking token root for token wallet",
                    );
                    let event_decoded_data = EvmTvmEventContract(&contract).event_decoded_data()?;

                    let token_root = event_decoded_data.token.address();
                    let token_root = UInt256::from_be_bytes(&token_root.get_bytestring(0));
                    let root_contract = tvm_subscriber.wait_contract_state(&token_root).await?;
                    #[cfg(feature = "ton")]
                    let proxy_wallet_address = JettonMinterContract(&root_contract)
                        .get_wallet_address(&event_decoded_data.proxy)?;
                    #[cfg(not(feature = "ton"))]
                    let proxy_wallet_address =
                        TokenRootContract(&root_contract).wallet_of(&event_decoded_data.proxy)?;

                    if event_decoded_data.token_wallet != proxy_wallet_address {
                        let proxy = UInt256::from_be_bytes(
                            &event_decoded_data.proxy.address().get_bytestring(0),
                        );
                        let expected = UInt256::from_be_bytes(
                            &proxy_wallet_address.address().get_bytestring(0),
                        );
                        let actual = UInt256::from_be_bytes(
                            &event_decoded_data.token_wallet.address().get_bytestring(0),
                        );
                        tracing::error!(
                            event = %DisplayAddr(account),
                            chain_id,
                            proxy = %DisplayAddr(proxy),
                            token_root = %DisplayAddr(token_root),
                            expected_token_wallet = %DisplayAddr(expected),
                            actual_token_wallet = %DisplayAddr(actual),
                            "EVM->TVM wrong token wallet for given token root",
                        );
                        preliminary_checks_succeeded = false;
                    }
                }

                // Get required subscriber
                match evm_subscribers.get_subscriber(chain_id) {
                    Some(subscriber) => (
                        subscriber,
                        event_emitter,
                        abi,
                        blocks_to_confirm,
                        preliminary_checks_succeeded,
                    ),
                    None => {
                        tracing::error!(
                            event = %DisplayAddr(account),
                            chain_id,
                            "EVM->TVM subscriber not found for event",
                        );
                        self.evm_tvm_events_state.remove(&account);
                        return Ok(());
                    }
                }
            }
            // Configuration not found
            None => {
                tracing::error!(
                    event = %DisplayAddr(account),
                    configuration = %DisplayAddr(event_init_data.configuration),
                    "EVM->TVM event configuration not found for event",
                );
                self.evm_tvm_events_state.remove(&account);
                return Ok(());
            }
        };

        let account_addr = ton_block::MsgAddrStd::with_address(None, 0, account.into());

        // Verify EVM event and create message to event contract
        let message = match evm_subscriber
            .verify(
                event_init_data.vote_data,
                event_emitter,
                event_abi,
                blocks_to_confirm,
                preliminary_checks_succeeded,
            )
            .await
        {
            // Confirm event if transaction was found
            Ok(VerificationStatus::Exists) => {
                UnsignedMessage::new(evm_tvm_event_contract::confirm(), account).arg(account_addr)
            }
            // Reject event if transaction not found
            Ok(VerificationStatus::NotExists { reason }) => {
                tracing::warn!(
                    event = %DisplayAddr(account),
                    configuration = %DisplayAddr(event_init_data.configuration),
                    reason,
                    "rejecting EVM->TVM event",
                );

                UnsignedMessage::new(evm_tvm_event_contract::reject(), account).arg(account_addr)
            }
            // Skip event otherwise
            Err(e) => {
                tracing::error!(event = %DisplayAddr(account), "failed to verify EVM->TVM event: {e:?}");
                self.evm_tvm_events_state.remove(&account);
                return Ok(());
            }
        };

        // Clone events observer and deliver message to the contract
        let evm_tvm_event_observer = match self.evm_tvm_events_state.pending.get(&account) {
            Some(entry) => entry.observer.clone(),
            None => return Ok(()),
        };
        let evm_tvm_events_state = Arc::downgrade(&self.evm_tvm_events_state);

        self.context
            .deliver_message(
                evm_tvm_event_observer,
                message,
                // Stop voting for the contract if it was removed
                move || match evm_tvm_events_state.upgrade() {
                    Some(state) => state.pending.contains_key(&account),
                    None => false,
                },
            )
            .await?;
        Ok(())
    }

    async fn update_tvm_evm_event(self: Arc<Self>, account: UInt256) -> Result<()> {
        if !self.tvm_evm_events_state.start_processing(&account) {
            return Ok(());
        }

        let keystore = &self.context.keystore;
        let tvm_subscriber = &self.context.tvm_subscriber;

        // Wait contract state
        let contract = tvm_subscriber.wait_contract_state(&account).await?;
        let base_event_contract = EventBaseContract(&contract);

        // Check further steps based on event statuses
        match base_event_contract.process(keystore.tvm.public_key(), true)? {
            EventAction::Nop => return Ok(()),
            EventAction::Remove => {
                self.tvm_evm_events_state.remove(&account);
                return Ok(());
            }
            EventAction::Vote => { /* continue voting */ }
        }
        let round_number = base_event_contract.round_number()?;

        // Get event details
        let event_init_data = TvmEvmEventContract(&contract).event_init_data()?;

        struct ConfigData {
            proxy: [u8; 20],
            data: Result<Vec<ton_abi::Token>>,
            #[cfg(feature = "ton")]
            verify_token_meta: bool,
        }

        // Find suitable configuration
        // NOTE: be sure to drop `self.state` lock before removing pending ton event.
        // It may deadlock otherwise!
        let data = {
            let state = self.state.read().await;
            state
                .tvm_evm_event_configurations
                .get(&event_init_data.configuration)
                .map(|configuration| ConfigData {
                    proxy: configuration.details.network_configuration.proxy,
                    data: ton_types::SliceData::load_cell(
                        event_init_data.vote_data.event_data.clone(),
                    )
                    .and_then(|cursor| {
                        TokenValue::decode_params(
                            &configuration.event_abi,
                            cursor,
                            &LATEST_ABI_VERSION,
                            false,
                        )
                    }),
                    #[cfg(feature = "ton")]
                    verify_token_meta: configuration.context.verify_token_meta,
                })
        };

        let decoded_data = match data {
            // Decode event data with event abi from configuration
            Some(ConfigData {
                proxy,
                data,
                #[cfg(feature = "ton")]
                verify_token_meta,
            }) => {
                #[allow(unused_mut)]
                let mut verification_error = None;
                #[cfg(feature = "ton")]
                if verify_token_meta {
                    tracing::info!(
                        event = %DisplayAddr(account),
                        "TVM->EVM checking token meta",
                    );
                    let event_decoded_data = TvmEvmEventContract(&contract).event_decoded_data()?;
                    let expected_meta = self
                        .context
                        .tokens_meta_client
                        .get_token_meta(&event_decoded_data.token.to_string())
                        .await?;

                    let mut meta_mismatch = false;
                    if event_decoded_data.name != expected_meta.name {
                        tracing::error!(
                            event = %DisplayAddr(account),
                            expected_token_name = expected_meta.name,
                            actual_token_name = event_decoded_data.name,
                            "TVM->EVM token name mismatch",
                        );
                        meta_mismatch = true;
                    }
                    if event_decoded_data.symbol != expected_meta.symbol {
                        tracing::error!(
                            event = %DisplayAddr(account),
                            expected_token_symbol = expected_meta.symbol,
                            actual_token_symbol = event_decoded_data.symbol,
                            "TVM->EVM token symbol mismatch",
                        );
                        meta_mismatch = true;
                    }
                    if event_decoded_data.decimals != expected_meta.decimals {
                        tracing::error!(
                            event = %DisplayAddr(account),
                            expected_token_decimals = expected_meta.decimals,
                            actual_token_decimals = event_decoded_data.decimals,
                            "TVM->EVM token decimals mismatch",
                        );
                        meta_mismatch = true;
                    }

                    if meta_mismatch {
                        verification_error = Some(BridgeError::TokenMetadataMismatch.into());
                    }
                }

                if let Some(err) = verification_error {
                    Err(err)
                } else {
                    data.and_then(|data| {
                        Ok(make_mapped_tvm_event(
                            event_init_data.vote_data.event_transaction_lt,
                            event_init_data.vote_data.event_timestamp,
                            map_tvm_tokens_to_evm_bytes(data)?,
                            event_init_data.configuration,
                            account,
                            proxy,
                            round_number,
                        ))
                    })
                }
            }
            // Do nothing when configuration was not found
            None => {
                tracing::error!(
                    event = %DisplayAddr(account),
                    configuration = %DisplayAddr(event_init_data.configuration),
                    "TVM->EVM event configuration not found for event",
                );
                self.tvm_evm_events_state.remove(&account);
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
                UnsignedMessage::new(tvm_evm_event_contract::confirm(), account)
                    .arg(keystore.evm.sign(&data).to_vec())
                    .arg(account_addr)
            }

            // Reject if event data is invalid
            Err(e) => {
                tracing::warn!(
                    event = %DisplayAddr(account),
                    "failed to compute vote data signature: {e:?}",
                );
                UnsignedMessage::new(tvm_evm_event_contract::reject(), account).arg(account_addr)
            }
        };

        // Clone events observer and deliver message to the contract
        let tvm_evm_event_observer = match self.tvm_evm_events_state.pending.get(&account) {
            Some(entry) => entry.observer.clone(),
            None => return Ok(()),
        };
        let tvm_evm_events_state = Arc::downgrade(&self.tvm_evm_events_state);

        self.context
            .deliver_message(
                tvm_evm_event_observer,
                message,
                // Stop voting for the contract if it was removed
                move || match tvm_evm_events_state.upgrade() {
                    Some(state) => state.pending.contains_key(&account),
                    None => false,
                },
            )
            .await?;
        Ok(())
    }

    async fn update_svm_tvm_event(self: Arc<Self>, account: UInt256) -> Result<()> {
        let svm_subscriber = match &self.context.svm_subscriber {
            // Continue only of SVM subscriber is enabled, and it is the first time we started processing this event
            Some(subscriber) if self.svm_tvm_events_state.start_processing(&account) => subscriber,
            _ => return Ok(()),
        };

        let keystore = &self.context.keystore;
        let tvm_subscriber = &self.context.tvm_subscriber;

        // Wait contract state
        let contract = tvm_subscriber.wait_contract_state(&account).await?;

        match EventBaseContract(&contract).process(keystore.tvm.public_key(), false)? {
            EventAction::Nop => return Ok(()),
            EventAction::Remove => {
                self.svm_tvm_events_state.remove(&account);
                return Ok(());
            }
            EventAction::Vote => { /* continue voting */ }
        }

        let event_init_data = SvmTvmEventContract(&contract).event_init_data()?;

        // Find suitable configuration
        // NOTE: be sure to drop `self.state` lock before removing pending ton event.
        // It may deadlock otherwise!
        let data = {
            let state = self.state.read().await;
            state
                .svm_tvm_event_configurations
                .get(&event_init_data.configuration)
                .map(|configuration| {
                    (
                        configuration.details.network_configuration.program,
                        ton_types::SliceData::load_cell(
                            event_init_data.vote_data.event_data.clone(),
                        )
                        .and_then(|cursor| {
                            TokenValue::decode_params(
                                &configuration.event_abi,
                                cursor,
                                &LATEST_ABI_VERSION,
                                false,
                            )
                        }),
                    )
                })
        };

        let (program, decoded_event_data) = match data {
            // Decode event data with event abi from configuration
            Some((program, data)) => (
                program,
                data.and_then(|data| evm_tvm_abi_converter::borsh::serialize(&data))?,
            ),
            // Do nothing when configuration was not found
            None => {
                tracing::error!(
                    event = %DisplayAddr(account),
                    configuration = %DisplayAddr(event_init_data.configuration),
                    "SVM->TVM event configuration not found for event",
                );
                self.svm_tvm_events_state.remove(&account);
                return Ok(());
            }
        };

        let signature =
            solana_sdk::signature::Signature::from_str(&event_init_data.vote_data.signature)?;

        let program_id = Pubkey::new_from_array(program.inner());

        let transaction_data = SvmTvmTransactionData {
            program_id,
            signature,
            slot: event_init_data.vote_data.slot,
            block_time: event_init_data.vote_data.block_time as i64,
            seed: event_init_data.vote_data.account_seed,
        };

        let account_data = SvmTvmAccountData {
            program_id,
            seed: event_init_data.vote_data.account_seed,
            event_data: decoded_event_data,
        };

        let account_addr = ton_block::MsgAddrStd::with_address(None, 0, account.into());

        // Verify SVM->TVM event and create message to event contract
        let message = match svm_subscriber
            .verify_svm_tvm_event(transaction_data, account_data)
            .await
        {
            // Confirm event if transaction was found
            Ok(VerificationStatus::Exists) => {
                UnsignedMessage::new(svm_tvm_event_contract::confirm(), account).arg(account_addr)
            }
            // Reject event if transaction not found
            Ok(VerificationStatus::NotExists { reason }) => {
                tracing::warn!(
                    event = %DisplayAddr(account),
                    configuration = %DisplayAddr(event_init_data.configuration),
                    reason,
                    "rejecting SVM->TVM event",
                );

                UnsignedMessage::new(svm_tvm_event_contract::reject(), account).arg(account_addr)
            }
            // Skip event otherwise
            Err(e) => {
                tracing::error!(
                    event = %DisplayAddr(account),
                    "failed to verify SVM->TVM event: {e:?}",
                );
                self.svm_tvm_events_state.remove(&account);
                return Ok(());
            }
        };

        // Clone events observer and deliver message to the contract
        let svm_tvm_event_observer = match self.svm_tvm_events_state.pending.get(&account) {
            Some(entry) => entry.observer.clone(),
            None => return Ok(()),
        };
        let svm_tvm_events_state = Arc::downgrade(&self.svm_tvm_events_state);

        self.context
            .deliver_message(
                svm_tvm_event_observer,
                message,
                // Stop voting for the contract if it was removed
                move || match svm_tvm_events_state.upgrade() {
                    Some(state) => state.pending.contains_key(&account),
                    None => false,
                },
            )
            .await?;

        Ok(())
    }

    async fn update_tvm_svm_event(self: Arc<Self>, account: UInt256) -> Result<()> {
        let svm_subscriber = match &self.context.svm_subscriber {
            // Continue only of SVM subscriber is enabled, and it is the first time we started processing this event
            Some(subscriber) if self.tvm_svm_events_state.start_processing(&account) => subscriber,
            _ => return Ok(()),
        };

        let keystore = &self.context.keystore;
        let tvm_subscriber = &self.context.tvm_subscriber;

        // Wait contract state
        let contract = tvm_subscriber.wait_contract_state(&account).await?;
        let base_event_contract = EventBaseContract(&contract);

        // Check further steps based on event statuses
        match base_event_contract.process(keystore.tvm.public_key(), true)? {
            EventAction::Nop => return Ok(()),
            EventAction::Remove => {
                self.tvm_svm_events_state.remove(&account);
                return Ok(());
            }
            EventAction::Vote => { /* continue voting */ }
        }
        let round_number = base_event_contract.round_number()?;
        let created_at = base_event_contract.created_at()?;

        // Get event details
        let event_init_data = TvmSvmEventContract(&contract).event_init_data()?;

        // Find suitable configuration
        // NOTE: be sure to drop `self.state` lock before removing pending ton event.
        // It may deadlock otherwise!
        let data = {
            let state = self.state.read().await;
            state
                .tvm_svm_event_configurations
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
                        ton_types::SliceData::load_cell(
                            event_init_data.vote_data.event_data.clone(),
                        )
                        .and_then(|cursor| {
                            TokenValue::decode_params(
                                &configuration.event_abi,
                                cursor,
                                &LATEST_ABI_VERSION,
                                false,
                            )
                        }),
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
                data.and_then(|data| evm_tvm_abi_converter::borsh::serialize(&data))?,
            ),
            // Do nothing when configuration was not found
            None => {
                tracing::error!(
                    event = %DisplayAddr(account),
                    configuration = %DisplayAddr(event_init_data.configuration),
                    "TVM->SVM event configuration not found for event",
                );
                self.tvm_svm_events_state.remove(&account);
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

        let voter_pubkey = self.context.keystore.svm.public_key();

        let account_addr = ton_block::MsgAddrStd::with_address(None, 0, account.into());

        let (svm_message_vote, svm_message_execute, svm_message_execute_payload, tvm_message) =
            match svm_subscriber
                .verify_tvm_svm_event(proposal_pubkey, decoded_event_data, created_at)
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

                    let svm_message_vote =
                        solana_sdk::message::Message::new(&[vote_ix], Some(&voter_pubkey));

                    let mut svm_message_execute = None;
                    let mut svm_message_execute_payload = None;
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

                        svm_message_execute = Some(solana_sdk::message::Message::new(
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

                            svm_message_execute_payload = Some(solana_sdk::message::Message::new(
                                &[execute_ix],
                                Some(&voter_pubkey),
                            ));
                        }
                    }

                    let ton_message =
                        UnsignedMessage::new(tvm_svm_event_contract::confirm(), account)
                            .arg(account_addr);

                    (
                        svm_message_vote,
                        svm_message_execute,
                        svm_message_execute_payload,
                        ton_message,
                    )
                }
                Ok(VerificationStatus::NotExists { reason }) => {
                    tracing::warn!(
                        event = %DisplayAddr(account),
                        configuration = %DisplayAddr(event_init_data.configuration),
                        reason,
                        "rejecting TVM->SVM event",
                    );

                    let ix = solana_bridge::instructions::vote_for_proposal_ix(
                        program_id,
                        instruction,
                        &voter_pubkey,
                        &proposal_pubkey,
                        round_number,
                        solana_bridge::bridge_types::Vote::Reject,
                    );
                    let svm_message = solana_sdk::message::Message::new(&[ix], Some(&voter_pubkey));

                    let tvm_message =
                        UnsignedMessage::new(tvm_svm_event_contract::reject(), account)
                            .arg(account_addr);

                    (svm_message, None, None, tvm_message)
                }
                // Skip event otherwise
                Err(e) => {
                    tracing::error!(
                        event = %DisplayAddr(account),
                        "failed to verify TVM->SVM event: {e:?}",
                    );
                    self.tvm_svm_events_state.remove(&account);
                    return Ok(());
                }
            };

        let rpc_client = svm_subscriber.get_rpc_client()?;

        if !svm_subscriber
            .is_already_voted(rpc_client, round_number, &proposal_pubkey, &voter_pubkey)
            .await?
        {
            // Extract vote and log it
            let c_ix = svm_message_vote.instructions.first().trust_me();
            let ix = solana_bridge::instructions::VoteForProposal::try_from_slice(&c_ix.data)?;

            tracing::info!(
                vote = ?ix.vote,
                %proposal_pubkey,
                "voting for SVM proposal...",
            );

            // Send confirm/reject to SVM
            let signature = svm_subscriber
                .send_message(rpc_client, svm_message_vote, &self.context.keystore)
                .await
                .map_err(parse_client_error)?;

            svm_subscriber
                .get_signature_status(rpc_client, &signature)
                .await
                .map_err(parse_client_error)?;

            tracing::info!(
                vote = ?ix.vote,
                %proposal_pubkey,
                "vote was sent",
            );

            // Execute proposal
            if let Some(message) = svm_message_execute {
                tracing::info!(
                    %proposal_pubkey,
                    "executing proposal...",
                );

                match svm_subscriber
                    .send_message(rpc_client, message, &self.context.keystore)
                    .await
                    .map_err(parse_client_error)
                {
                    Ok(_) => {
                        tracing::info!(
                            %proposal_pubkey,
                            "proposal was executed",
                        );

                        // Execute payload
                        if let Some(message) = svm_message_execute_payload {
                            tracing::info!(
                                %proposal_pubkey,
                                "executing payload...",
                            );

                            match svm_subscriber
                                .send_message(rpc_client, message, &self.context.keystore)
                                .await
                                .map_err(parse_client_error)
                            {
                                Ok(_) => {
                                    tracing::info!(
                                        %proposal_pubkey,
                                        "payload was executed",
                                    );
                                }
                                Err(e) => {
                                    tracing::error!(
                                        %proposal_pubkey,
                                        "failed to execute SVM payload: {e:?}",
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            %proposal_pubkey,
                            "failed to execute SVM proposal: {e:?}",
                        );
                    }
                }
            }
        }

        // Clone events observer and deliver message to the contract
        let tvm_svm_event_observer = match self.tvm_svm_events_state.pending.get(&account) {
            Some(entry) => entry.observer.clone(),
            None => return Ok(()),
        };
        let tvm_svm_events_state = Arc::downgrade(&self.tvm_svm_events_state);

        self.context
            .deliver_message(
                tvm_svm_event_observer,
                tvm_message,
                // Stop voting for the contract if it was removed
                move || match tvm_svm_events_state.upgrade() {
                    Some(state) => state.pending.contains_key(&account),
                    None => false,
                },
            )
            .await?;

        Ok(())
    }

    async fn check_connector_contract(&self, connector_account: UInt256) -> Result<()> {
        let tvm_subscriber = &self.context.tvm_subscriber;

        // Get event configuration address
        let event_configuration = {
            // Wait until connector contract state is found
            let contract = tvm_subscriber
                .wait_contract_state(&connector_account)
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
        let contract = tvm_subscriber
            .wait_contract_state(&event_configuration)
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
        )
        .await?;

        Ok(())
    }

    async fn get_all_configurations(&self) -> Result<()> {
        // Lock state before other logic to make sure that all events
        // will be queued in their handlers
        let mut state = self.state.write().await;

        let tvm_subscriber = &self.context.tvm_subscriber;

        let contract = tvm_subscriber
            .get_contract_state(&self.bridge_account)
            .await
            .context("Failed to get bridge account state")?
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
            let details = match tvm_subscriber
                .get_contract_state(&connector_account)
                .await
                .context("Failed to get connector account state")?
            {
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
            tvm_subscriber
                .add_transactions_subscription([connector_account], &observer)
                .await;

            // Skip event configuration if it is disabled
            if !enabled {
                continue;
            }

            // Find event configuration contract
            let configuration_contract = match tvm_subscriber
                .get_contract_state(&configuration_account)
                .await
                .context("Failed to get configuration state")?
            {
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
            if let Err(e) = self
                .process_event_configuration(
                    &mut state,
                    &connector_account,
                    &configuration_account,
                    &configuration_contract,
                )
                .await
            {
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
    async fn process_event_configuration(
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
            // Extract and populate EVM->TVM event configuration details
            EventType::EvmTvm => self
                .add_evm_tvm_event_configuration(
                    state,
                    configuration_account,
                    configuration_contract,
                )
                .await
                .context("Failed to add EVM->TVM event configuration")?,
            // Extract and populate TVM->EVM event configuration details
            EventType::TvmEvm => self
                .add_tvm_evm_event_configuration(
                    state,
                    configuration_account,
                    configuration_contract,
                )
                .await
                .context("Failed to add TVM->EVM event configuration")?,
            // Extract and populate SVM->TVM event configuration details
            EventType::SvmTvm => self
                .add_svm_tvm_event_configuration(
                    state,
                    configuration_account,
                    configuration_contract,
                )
                .await
                .context("Failed to add SVM->TVM event configuration")?,
            // Extract and populate TVM->SVM event configuration details
            EventType::TvmSvm => self
                .add_tvm_svm_event_configuration(
                    state,
                    configuration_account,
                    configuration_contract,
                )
                .await
                .context("Failed to add TVM->SVM event configuration")?,
            EventType::TvmTvm => {
                tracing::info!("skipping TVM->TVM event configuration");
            }
        };

        // Done
        Ok(())
    }

    async fn add_evm_tvm_event_configuration(
        &self,
        state: &mut BridgeState,
        account: &UInt256,
        contract: &ExistingContract,
    ) -> Result<()> {
        let flags = EventConfigurationBaseContract(contract)
            .get_flags()
            .context("Failed to get EVM->TVM event configuration flags")?;

        // Get configuration details
        let details = EvmTvmEventConfigurationContract(contract)
            .get_details()
            .context("Failed to get EVM->TVM event configuration details")?;

        let ctx = flags
            .map(|flags| EvmToTvmMappingContext::from(flags as u8))
            .unwrap_or_default();

        // Verify and prepare abi
        let event_abi = Arc::new(EvmEventAbi::new(
            &details.basic_configuration.event_abi,
            ctx,
        )?);
        let topic_hash = event_abi.get_eth_topic_hash().to_fixed_bytes();
        let evm_contract_address = details.network_configuration.event_emitter;

        // Get suitable EVM subscriber for specified chain id
        let evm_subscriber = self
            .context
            .evm_subscribers
            .get_subscriber(details.network_configuration.chain_id)
            .ok_or(BridgeError::UnknownChainId)?;

        // Add unique event hash
        add_event_code_hash(
            &mut state.event_code_hashes,
            &details.basic_configuration.event_code,
            EventType::EvmTvm,
        )?;

        // Add configuration entry
        let observer = AccountObserver::new(&self.evm_tvm_event_configurations_tx);
        match state.evm_tvm_event_configurations.entry(*account) {
            hash_map::Entry::Vacant(entry) => {
                tracing::info!(
                    configuration = %DisplayAddr(account),
                    ?details, "added new EVM->TVM event configuration"
                );

                self.total_active_evm_tvm_event_configurations
                    .fetch_add(1, Ordering::Release);

                entry.insert(EvmTvmEventConfigurationState {
                    details,
                    event_abi,
                    mapping_context: ctx,
                    _observer: observer.clone(),
                });
            }
            hash_map::Entry::Occupied(_) => {
                tracing::info!(
                    configuration = %DisplayAddr(account),
                    "EVM->TVM event configuration already exists",
                );
                return Err(BridgeError::EventConfigurationAlreadyExists.into());
            }
        };

        // Subscribe to EVM events
        evm_subscriber.subscribe(evm_contract_address.into(), topic_hash, *account);

        // Subscribe to TVM events
        self.context
            .tvm_subscriber
            .add_transactions_subscription([*account], &observer)
            .await;

        // Done
        Ok(())
    }

    async fn add_tvm_evm_event_configuration(
        &self,
        state: &mut BridgeState,
        account: &UInt256,
        contract: &ExistingContract,
    ) -> Result<()> {
        #[cfg(feature = "ton")]
        let flags = EventConfigurationBaseContract(contract)
            .get_flags()
            .context("Failed to get TVM->EVM event configuration flags")?;

        // Get configuration details
        let details = TvmEvmEventConfigurationContract(contract)
            .get_details()
            .context("Failed to get TVM->EVM event configuration details")?;

        #[cfg(feature = "ton")]
        let ctx = flags
            .map(|flags| TvmToEvmContext::from(flags as u8))
            .unwrap_or_default();

        // Check if configuration is expired
        let current_timestamp = self.context.tvm_subscriber.current_utime();
        if details.is_expired(current_timestamp) {
            // Do nothing in that case
            tracing::warn!(
                configuration = %DisplayAddr(account),
                current_timestamp,
                end_timestamp = details.network_configuration.end_timestamp,
                "ignoring disabled TVM->EVM event configuration",
            );
            return Ok(());
        };

        // Verify and prepare abi
        let event_abi = decode_tvm_event_abi(&details.basic_configuration.event_abi)?;

        // Add unique event hash
        add_event_code_hash(
            &mut state.event_code_hashes,
            &details.basic_configuration.event_code,
            EventType::TvmEvm,
        )?;

        // Add configuration entry
        let observer = AccountObserver::new(&self.tvm_evm_event_configurations_tx);
        match state.tvm_evm_event_configurations.entry(*account) {
            hash_map::Entry::Vacant(entry) => {
                tracing::info!(
                    configuration = %DisplayAddr(account),
                    ?details,
                    "added new TVM->EVM event configuration"
                );

                self.total_active_tvm_evm_event_configurations
                    .fetch_add(1, Ordering::Release);

                entry.insert(TvmEvmEventConfigurationState {
                    details,
                    #[cfg(feature = "ton")]
                    context: ctx,
                    event_abi,
                    _observer: observer.clone(),
                });
            }
            hash_map::Entry::Occupied(_) => {
                tracing::info!(
                    configuration = %DisplayAddr(account),
                    "TVM->EVM event configuration already exists",
                );
                return Err(BridgeError::EventConfigurationAlreadyExists.into());
            }
        };

        // Subscribe to TVM events
        self.context
            .tvm_subscriber
            .add_transactions_subscription([*account], &observer)
            .await;

        // Done
        Ok(())
    }

    async fn add_svm_tvm_event_configuration(
        &self,
        state: &mut BridgeState,
        account: &UInt256,
        contract: &ExistingContract,
    ) -> Result<()> {
        if self.context.svm_subscriber.is_none() {
            tracing::info!(
                configuration = %DisplayAddr(account),
                "ignoring SVM->TVM event configuration: SVM subscriber is disabled",
            );
            return Ok(());
        }

        // Get configuration details
        let details = SvmTvmEventConfigurationContract(contract)
            .get_details()
            .context("Failed to get SVM->TVM event configuration details")?;

        // Check if configuration is expired
        let current_timestamp = self.context.tvm_subscriber.current_utime();
        if details.is_expired(current_timestamp as u64) {
            // Do nothing in that case
            tracing::warn!(
                configuration = %DisplayAddr(account),
                current_timestamp,
                end_timestamp = details.network_configuration.end_timestamp,
                "ignoring disabled SVM->TVM event configuration",
            );
            return Ok(());
        };

        // Verify and prepare abi
        let event_abi = decode_tvm_event_abi(&details.basic_configuration.event_abi)?;

        // Add unique event hash
        add_event_code_hash(
            &mut state.event_code_hashes,
            &details.basic_configuration.event_code,
            EventType::SvmTvm,
        )?;

        // Add configuration entry
        let observer = AccountObserver::new(&self.svm_tvm_event_configurations_tx);
        match state.svm_tvm_event_configurations.entry(*account) {
            hash_map::Entry::Vacant(entry) => {
                tracing::info!(
                    configuration = %DisplayAddr(account),
                    ?details,
                    "added new SVM->TVM event configuration",
                );

                self.total_active_svm_tvm_event_configurations
                    .fetch_add(1, Ordering::Release);

                entry.insert(SvmTvmEventConfigurationState {
                    details,
                    event_abi,
                    _observer: observer.clone(),
                });
            }
            hash_map::Entry::Occupied(_) => {
                tracing::info!(
                    configuration = %DisplayAddr(account),
                    "SVM->TVM event configuration already exists",
                );
                return Err(BridgeError::EventConfigurationAlreadyExists.into());
            }
        };

        // Subscribe to TVM events
        self.context
            .tvm_subscriber
            .add_transactions_subscription([*account], &observer)
            .await;

        // Done
        Ok(())
    }

    async fn add_tvm_svm_event_configuration(
        &self,
        state: &mut BridgeState,
        account: &UInt256,
        contract: &ExistingContract,
    ) -> Result<()> {
        let svm_subscriber = match &self.context.svm_subscriber {
            Some(subscriber) => subscriber,
            None => {
                tracing::info!(
                    configuration = %DisplayAddr(account),
                    "ignoring TVM->SVM event configuration: SVM subscriber is disabled",
                );
                return Ok(());
            }
        };

        // Get configuration details
        let details = TvmSvmEventConfigurationContract(contract)
            .get_details()
            .context("Failed to get TVM->SVM event configuration details")?;

        // Check if configuration is expired
        let current_timestamp = self.context.tvm_subscriber.current_utime();
        if details.is_expired(current_timestamp) {
            // Do nothing in that case
            tracing::warn!(
                configuration = %DisplayAddr(account),
                current_timestamp,
                end_timestamp = details.network_configuration.end_timestamp,
                "ignoring disabled TVM->SVM event configuration",
            );
            return Ok(());
        };

        // Verify and prepare abi
        let event_abi = decode_tvm_event_abi(&details.basic_configuration.event_abi)?;

        // Add unique event hash
        add_event_code_hash(
            &mut state.event_code_hashes,
            &details.basic_configuration.event_code,
            EventType::TvmSvm,
        )?;

        // Get SVM program address to subscribe
        let program_pubkey = Pubkey::new_from_array(details.network_configuration.program.inner());

        // Add configuration entry
        let observer = AccountObserver::new(&self.tvm_svm_event_configurations_tx);
        match state.tvm_svm_event_configurations.entry(*account) {
            hash_map::Entry::Vacant(entry) => {
                tracing::info!(
                    configuration = %DisplayAddr(account),
                    ?details,
                    "added new TVM->SVM event configuration",
                );

                self.total_active_tvm_svm_event_configurations
                    .fetch_add(1, Ordering::Release);

                entry.insert(TvmSvmEventConfigurationState {
                    details,
                    event_abi,
                    _observer: observer.clone(),
                });
            }
            hash_map::Entry::Occupied(_) => {
                tracing::info!(
                    configuration = %DisplayAddr(account),
                    "TVM->SVM event configuration already exists"
                );
                return Err(BridgeError::EventConfigurationAlreadyExists.into());
            }
        };

        // Subscribe to SVM programs
        svm_subscriber.subscribe(program_pubkey);

        // Subscribe to TVM events
        self.context
            .tvm_subscriber
            .add_transactions_subscription([*account], &observer)
            .await;

        // Done
        Ok(())
    }

    async fn get_all_events(self: &Arc<Self>) -> Result<()> {
        type AccountsSet = FxHashSet<UInt256>;

        #[allow(clippy::too_many_arguments)]
        async fn iterate_events(
            bridge: Arc<Bridge>,
            code_hash: UInt256,
            event_type: EventType,
            unique_evm_tvm_event_configurations: Arc<AccountsSet>,
            unique_tvm_evm_event_configurations: Arc<AccountsSet>,
            unique_svm_tvm_event_configurations: Arc<AccountsSet>,
            unique_tvm_svm_event_configurations: Arc<AccountsSet>,
        ) -> Result<()> {
            let our_public_key = bridge.context.keystore.tvm.public_key();
            let has_svm_subscriber = bridge.context.svm_subscriber.is_some();

            let tvm_subscriber = &bridge.context.tvm_subscriber;
            let addresses = tvm_subscriber
                .get_accounts_by_code_hash(code_hash)
                .await
                .context("Failed to get accounts by code hash")?;

            for address in addresses {
                let hash = UInt256::from_be_bytes(&address.address().get_bytestring(0));

                let contract = tvm_subscriber
                    .get_contract_state(&hash)
                    .await?
                    .ok_or(BridgeError::AccountNotFound(hash.to_hex_string()))?;

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
                                continue;
                            }
                        }
                    };
                }

                // Process event
                match EventBaseContract(&contract)
                    .process(our_public_key, event_type == EventType::TvmEvm)
                {
                    Ok(EventAction::Nop | EventAction::Vote) => match event_type {
                        EventType::EvmTvm => {
                            let configuration = check_configuration!(EvmTvmEventContract);

                            if !unique_evm_tvm_event_configurations.contains(&configuration) {
                                tracing::warn!(
                                    event = %DisplayAddr(hash),
                                    configuration = %DisplayAddr(configuration),
                                    "EVM->TVM event configuration not found"
                                );
                                continue;
                            }

                            if bridge
                                .add_pending_event(hash, &bridge.evm_tvm_events_state)
                                .await
                            {
                                bridge.spawn_background_task(
                                    "initial update EVM->TVM event",
                                    bridge.clone().update_evm_tvm_event(hash),
                                );
                            }
                        }
                        EventType::TvmEvm => {
                            let configuration = check_configuration!(TvmEvmEventContract);

                            if !unique_tvm_evm_event_configurations.contains(&configuration) {
                                tracing::warn!(
                                    event = %DisplayAddr(hash),
                                    configuration = %DisplayAddr(configuration),
                                    "TVM->EVM event configuration not found",
                                );
                                continue;
                            }

                            if bridge
                                .add_pending_event(hash, &bridge.tvm_evm_events_state)
                                .await
                            {
                                bridge.spawn_background_task(
                                    "initial update TVM->EVM event",
                                    bridge.clone().update_tvm_evm_event(hash),
                                );
                            }
                        }
                        EventType::SvmTvm if has_svm_subscriber => {
                            let configuration = check_configuration!(SvmTvmEventContract);

                            if !unique_svm_tvm_event_configurations.contains(&configuration) {
                                tracing::warn!(
                                    event = %DisplayAddr(hash),
                                    configuration = %DisplayAddr(configuration),
                                    "SVM->TVM event configuration not found",
                                );
                                continue;
                            }

                            if bridge
                                .add_pending_event(hash, &bridge.svm_tvm_events_state)
                                .await
                            {
                                bridge.spawn_background_task(
                                    "initial update SVM->TVM event",
                                    bridge.clone().update_svm_tvm_event(hash),
                                );
                            }
                        }
                        EventType::TvmSvm if has_svm_subscriber => {
                            let configuration = check_configuration!(TvmSvmEventContract);

                            if !unique_tvm_svm_event_configurations.contains(&configuration) {
                                tracing::warn!(
                                    event = %DisplayAddr(hash),
                                    configuration = %DisplayAddr(configuration),
                                    "TVM->SVM event configuration not found",
                                );
                                continue;
                            }

                            if bridge
                                .add_pending_event(hash, &bridge.tvm_svm_events_state)
                                .await
                            {
                                bridge.spawn_background_task(
                                    "initial update TVM->SVM event",
                                    bridge.clone().update_tvm_svm_event(hash),
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
            }

            Ok(())
        }

        // Lock state to prevent adding new configurations
        let state = self.state.read().await;

        // Prepare shard task context
        let event_code_hashes = &state.event_code_hashes;

        // NOTE: configuration sets are explicitly constructed from state instead of
        // just using [evm/tvm/svm]_event_counters. It is done on purpose to use the actual
        // configurations. It is acceptable that event counters will not be relevant
        let unique_evm_tvm_event_configurations =
            Arc::new(state.unique_evm_tvm_event_configurations());
        let unique_tvm_evm_event_configurations =
            Arc::new(state.unique_tvm_evm_event_configurations());
        let unique_svm_tvm_event_configurations =
            Arc::new(state.unique_svm_tvm_event_configurations());
        let unique_tvm_svm_event_configurations =
            Arc::new(state.unique_tvm_svm_event_configurations());

        let start = std::time::Instant::now();

        tracing::info!("started searching for all events");
        let mut results_rx = {
            let (results_tx, results_rx) = mpsc::unbounded_channel();

            for (code_hash, event_type) in event_code_hashes {
                let code_hash = *code_hash;
                let event_type = *event_type;

                let bridge = self.clone();
                let results_tx = results_tx.clone();

                let unique_evm_tvm_event_configurations =
                    unique_evm_tvm_event_configurations.clone();
                let unique_tvm_evm_event_configurations =
                    unique_tvm_evm_event_configurations.clone();
                let unique_svm_tvm_event_configurations =
                    unique_svm_tvm_event_configurations.clone();
                let unique_tvm_svm_event_configurations =
                    unique_tvm_svm_event_configurations.clone();

                tokio::spawn(async move {
                    let start = std::time::Instant::now();
                    let result = iterate_events(
                        bridge,
                        code_hash,
                        event_type,
                        unique_evm_tvm_event_configurations,
                        unique_tvm_evm_event_configurations,
                        unique_svm_tvm_event_configurations,
                        unique_tvm_svm_event_configurations,
                    )
                    .await;
                    tracing::info!(
                        code_hash = %DisplayCodeHash(code_hash),
                        elapsed_sec = start.elapsed().as_secs(),
                        "processed accounts",
                    );
                    results_tx.send(result).ok();
                });
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
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;

                // Get bridge if it is still alive
                let bridge = match bridge.upgrade() {
                    Some(bridge) => bridge,
                    None => return,
                };
                let tvm_subscriber = &bridge.context.tvm_subscriber;

                // Get current time from masterchain
                let current_utime = tvm_subscriber.current_utime();

                // Check expired configurations
                let has_expired_tvm_evm_configurations = {
                    let state = bridge.state.read().await;
                    state.has_expired_tvm_evm_event_configurations(current_utime)
                };

                let has_expired_tvm_svm_configurations = {
                    let state = bridge.state.read().await;
                    state.has_expired_tvm_svm_event_configurations(current_utime)
                };

                let has_expired_svm_tvm_configurations = {
                    let state = bridge.state.read().await;
                    state.has_expired_svm_tvm_event_configurations(current_utime)
                };

                // Do nothing if there are not expired configurations
                if !has_expired_tvm_evm_configurations
                    && !has_expired_tvm_svm_configurations
                    && !has_expired_svm_tvm_configurations
                {
                    continue;
                }

                let mut state = bridge.state.write().await;

                // Remove TVM->EVM expired configurations
                let mut total_removed = 0;
                state.tvm_evm_event_configurations.retain(|account, state| {
                    if state.details.is_expired(current_utime) {
                        tracing::warn!(
                            configuration = %DisplayAddr(account),
                            current_utime,
                            "removing TVM->EVM event configuration",
                        );
                        total_removed += 1;
                        false
                    } else {
                        true
                    }
                });
                bridge
                    .total_active_tvm_evm_event_configurations
                    .fetch_sub(total_removed, Ordering::Release);

                // Remove TVM->SVM expired configurations
                let mut total_removed = 0;
                state.tvm_svm_event_configurations.retain(|account, state| {
                    if state.details.is_expired(current_utime) {
                        tracing::warn!(
                            configuration = %DisplayAddr(account),
                            current_utime,
                            "removing TVM->SVM event configuration",
                        );
                        total_removed += 1;
                        false
                    } else {
                        true
                    }
                });
                bridge
                    .total_active_tvm_svm_event_configurations
                    .fetch_sub(total_removed, Ordering::Release);

                // Remove SVM->TVM expired configurations
                let mut total_removed = 0;
                state.svm_tvm_event_configurations.retain(|account, state| {
                    if state.details.is_expired(current_utime as u64) {
                        tracing::warn!(
                            configuration = %DisplayAddr(account),
                            current_utime,
                            "removing SVM->TVM event configuration",
                        );
                        total_removed += 1;
                        false
                    } else {
                        true
                    }
                });
                bridge
                    .total_active_svm_tvm_event_configurations
                    .fetch_sub(total_removed, Ordering::Release);
            }
        });
    }

    /// Creates EVM event observer if it doesn't exist and subscribes it to transactions
    async fn add_pending_event<T>(&self, account: UInt256, state: &EventsState<T>) -> bool
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
                .tvm_subscriber
                .add_transactions_subscription([account], &observer)
                .await;
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
    pub pending_evm_tvm_event_count: usize,
    pub pending_tvm_evm_event_count: usize,
    pub pending_svm_tvm_event_count: usize,
    pub pending_tvm_svm_event_count: usize,
    pub total_active_evm_tvm_event_configurations: usize,
    pub total_active_tvm_evm_event_configurations: usize,
    pub total_active_svm_tvm_event_configurations: usize,
    pub total_active_tvm_svm_event_configurations: usize,
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

    async fn update_event(bridge: Arc<Bridge>, account: UInt256) -> Result<()>;
}

#[async_trait::async_trait]
impl EventExt for EvmTvmEvent {
    const REQUIRE_ALL_SIGNATURES: bool = false;

    async fn update_event(bridge: Arc<Bridge>, account: UInt256) -> Result<()> {
        bridge.update_evm_tvm_event(account).await
    }
}

#[async_trait::async_trait]
impl EventExt for TvmEvmEvent {
    const REQUIRE_ALL_SIGNATURES: bool = true;

    async fn update_event(bridge: Arc<Bridge>, account: UInt256) -> Result<()> {
        bridge.update_tvm_evm_event(account).await
    }
}

#[async_trait::async_trait]
impl EventExt for SvmTvmEvent {
    const REQUIRE_ALL_SIGNATURES: bool = false;

    async fn update_event(bridge: Arc<Bridge>, account: UInt256) -> Result<()> {
        bridge.update_svm_tvm_event(account).await
    }
}

#[async_trait::async_trait]
impl EventExt for TvmSvmEvent {
    const REQUIRE_ALL_SIGNATURES: bool = true;

    async fn update_event(bridge: Arc<Bridge>, account: UInt256) -> Result<()> {
        bridge.update_tvm_svm_event(account).await
    }
}

/// Semi-persistent bridge contracts collection
#[derive(Default)]
struct BridgeState {
    connectors: ConnectorsMap,
    evm_tvm_event_configurations: EvmTvmEventConfigurationsMap,
    tvm_evm_event_configurations: TvmEvmEventConfigurationsMap,
    svm_tvm_event_configurations: SvmTvmEventConfigurationsMap,
    tvm_svm_event_configurations: TvmSvmEventConfigurationsMap,

    /// Unique event contracts code hashes.
    ///
    /// NOTE: only built on startup and then updated on each new configuration.
    /// Elements are not removed because it is not needed (the situation when one
    /// contract code will be used for EVM and TVM simultaneously)
    event_code_hashes: EventCodeHashesMap,
}

impl BridgeState {
    fn has_expired_tvm_evm_event_configurations(&self, current_timestamp: u32) -> bool {
        self.tvm_evm_event_configurations
            .iter()
            .any(|(_, state)| state.details.is_expired(current_timestamp))
    }

    fn has_expired_tvm_svm_event_configurations(&self, current_timestamp: u32) -> bool {
        self.tvm_svm_event_configurations
            .iter()
            .any(|(_, state)| state.details.is_expired(current_timestamp))
    }

    fn has_expired_svm_tvm_event_configurations(&self, current_timestamp: u32) -> bool {
        self.svm_tvm_event_configurations
            .iter()
            .any(|(_, state)| state.details.is_expired(current_timestamp as u64))
    }

    fn unique_evm_tvm_event_configurations(&self) -> FxHashSet<UInt256> {
        self.evm_tvm_event_configurations.keys().copied().collect()
    }

    fn unique_tvm_evm_event_configurations(&self) -> FxHashSet<UInt256> {
        self.tvm_evm_event_configurations.keys().copied().collect()
    }

    fn unique_svm_tvm_event_configurations(&self) -> FxHashSet<UInt256> {
        self.svm_tvm_event_configurations.keys().copied().collect()
    }

    fn unique_tvm_svm_event_configurations(&self) -> FxHashSet<UInt256> {
        self.tvm_svm_event_configurations.keys().copied().collect()
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

impl EventBaseContract<'_> {
    /// Determine event action
    fn process(&self, public_key: &UInt256, require_all_signatures: bool) -> Result<EventAction> {
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
            // Special case for TVM->EVM event which must collect as many signatures as possible
            EventStatus::Confirmed
                if require_all_signatures
                    && self.0.account.storage.balance.grams.as_u128() > 0
                    && self.get_voters(EventVote::Empty)?.contains(public_key)
                    && self.get_api_version().unwrap_or_default() == SUPPORTED_API_VERSION =>
            {
                EventAction::Vote
            }
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
}

/// EVM->TVM event configuration data
#[derive(Clone)]
struct EvmTvmEventConfigurationState {
    /// Configuration details
    details: EvmTvmEventConfigurationDetails,
    /// Mapping context
    mapping_context: EvmToTvmMappingContext,
    /// Parsed and mapped event ABI
    event_abi: Arc<EvmEventAbi>,

    /// Observer must live as long as configuration lives
    _observer: Arc<AccountObserver<EvmTvmEventConfigurationEvent>>,
}

/// TVM->EVM event configuration data
#[derive(Clone)]
struct TvmEvmEventConfigurationState {
    /// Configuration details
    details: TvmEvmEventConfigurationDetails,
    /// Context
    #[cfg(feature = "ton")]
    context: TvmToEvmContext,
    /// Parsed `eventData` ABI
    event_abi: Vec<ton_abi::Param>,

    /// Observer must live as long as configuration lives
    _observer: Arc<AccountObserver<TvmEvmEventConfigurationEvent>>,
}

impl TvmEvmEventConfigurationDetails {
    fn is_expired(&self, current_timestamp: u32) -> bool {
        (1..current_timestamp).contains(&self.network_configuration.end_timestamp)
    }
}

/// EVM->TVM event configuration data
#[derive(Clone)]
struct SvmTvmEventConfigurationState {
    /// Configuration details
    details: SvmTvmEventConfigurationDetails,
    /// Parsed and mapped event ABI
    event_abi: Vec<ton_abi::Param>,

    /// Observer must live as long as configuration lives
    _observer: Arc<AccountObserver<SvmTvmEventConfigurationEvent>>,
}

impl SvmTvmEventConfigurationDetails {
    fn is_expired(&self, current_timestamp: u64) -> bool {
        (1..current_timestamp).contains(&self.network_configuration.end_timestamp)
    }
}

/// TVM->SVM event configuration data
#[derive(Clone)]
struct TvmSvmEventConfigurationState {
    /// Configuration details
    details: TvmSvmEventConfigurationDetails,
    /// Parsed `eventData` ABI
    event_abi: Vec<ton_abi::Param>,

    /// Observer must live as long as configuration lives
    _observer: Arc<AccountObserver<TvmSvmEventConfigurationEvent>>,
}

impl TvmSvmEventConfigurationDetails {
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
}

#[derive(Debug, Clone)]
enum TvmEvmEventConfigurationEvent {
    EventDeployed { address: UInt256 },
    SetEndTimestamp { end_timestamp: u32 },
}

impl ReadFromTransaction for TvmEvmEventConfigurationEvent {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        let in_msg_body = ctx.in_msg_internal()?.body()?;

        let set_end_timestamp = tvm_evm_event_configuration_contract::set_end_timestamp();

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
enum EvmTvmEventConfigurationEvent {
    EventsDeployed { events: Vec<UInt256> },
    SetEndBlockNumber { end_block_number: u32 },
}

impl ReadFromTransaction for EvmTvmEventConfigurationEvent {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        let in_msg_body = ctx.in_msg_internal()?.body()?;

        let set_end_block_number = evm_tvm_event_configuration_contract::set_end_block_number();

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
enum TvmSvmEventConfigurationEvent {
    EventDeployed { address: UInt256 },
    SetEndTimestamp { end_timestamp: u32 },
}

impl ReadFromTransaction for TvmSvmEventConfigurationEvent {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        let in_msg_body = ctx.in_msg_internal()?.body()?;

        let set_end_timestamp = tvm_svm_event_configuration_contract::set_end_timestamp();

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
enum SvmTvmEventConfigurationEvent {
    EventsDeployed { events: Vec<UInt256> },
    SetEndTimestamp { end_timestamp: u64 },
}

impl ReadFromTransaction for SvmTvmEventConfigurationEvent {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        let in_msg_body = ctx.in_msg_internal()?.body()?;

        let set_end_timestamp = svm_tvm_event_configuration_contract::set_end_timestamp();

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
                if events.is_empty() {
                    return None;
                }
                Some(Self::EventsDeployed { events })
            }
        }
    }
}

impl ReadFromTransaction for EventStatus {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        EventBaseContract(ctx.account_state).status().ok()
    }
}

#[derive(Debug, Clone)]
enum EvmTvmEvent {
    ReceiveRoundRelays { keys: Vec<UInt256> },
    Confirm { public_key: UInt256 },
    Reject { public_key: UInt256 },
    Rejected,
}

impl ReadFromTransaction for EvmTvmEvent {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        if has_rejected_event(ctx) {
            return Some(Self::Rejected);
        }

        let in_msg = ctx.in_msg;
        match in_msg.header() {
            ton_block::CommonMsgInfo::ExtInMsgInfo(_) => {
                let (public_key, body) = read_external_in_msg(&in_msg.body()?)?;

                match read_function_id(&body) {
                    Ok(id) if id == evm_tvm_event_contract::confirm().input_id => {
                        Some(EvmTvmEvent::Confirm { public_key })
                    }
                    Ok(id) if id == evm_tvm_event_contract::reject().input_id => {
                        Some(EvmTvmEvent::Reject { public_key })
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

                        Some(EvmTvmEvent::ReceiveRoundRelays { keys: items })
                    }
                    _ => None,
                }
            }
            ton_block::CommonMsgInfo::ExtOutMsgInfo(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
enum TvmEvmEvent {
    ReceiveRoundRelays { keys: Vec<UInt256> },
    Confirm { public_key: UInt256 },
    Reject { public_key: UInt256 },
    Rejected,
    Closed,
}

impl ReadFromTransaction for TvmEvmEvent {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        if has_rejected_event(ctx) {
            return Some(Self::Rejected);
        }

        let in_msg = ctx.in_msg;
        let event = match in_msg.header() {
            ton_block::CommonMsgInfo::ExtInMsgInfo(_) => {
                let (public_key, body) = read_external_in_msg(&in_msg.body()?)?;

                match read_function_id(&body) {
                    Ok(id) if id == tvm_evm_event_contract::confirm().input_id => {
                        Some(TvmEvmEvent::Confirm { public_key })
                    }
                    Ok(id) if id == tvm_evm_event_contract::reject().input_id => {
                        Some(TvmEvmEvent::Reject { public_key })
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

                        Some(TvmEvmEvent::ReceiveRoundRelays { keys: items })
                    }
                    _ => None,
                }
            }
            ton_block::CommonMsgInfo::ExtOutMsgInfo(_) => None,
        };

        if event.is_none() {
            let balance = ctx.account_state.account.storage.balance.grams;
            if balance.as_u128() == 0 {
                return Some(Self::Closed);
            }
        }

        event
    }
}

#[derive(Debug, Clone)]
enum SvmTvmEvent {
    ReceiveRoundRelays { keys: Vec<UInt256> },
    Confirm { public_key: UInt256 },
    Reject { public_key: UInt256 },
    Rejected,
}

impl ReadFromTransaction for SvmTvmEvent {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        if has_rejected_event(ctx) {
            return Some(Self::Rejected);
        }

        let in_msg = ctx.in_msg;
        match in_msg.header() {
            ton_block::CommonMsgInfo::ExtInMsgInfo(_) => {
                let (public_key, body) = read_external_in_msg(&in_msg.body()?)?;

                match read_function_id(&body) {
                    Ok(id) if id == svm_tvm_event_contract::confirm().input_id => {
                        Some(SvmTvmEvent::Confirm { public_key })
                    }
                    Ok(id) if id == svm_tvm_event_contract::reject().input_id => {
                        Some(SvmTvmEvent::Reject { public_key })
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

                        Some(SvmTvmEvent::ReceiveRoundRelays { keys: items })
                    }
                    _ => None,
                }
            }
            ton_block::CommonMsgInfo::ExtOutMsgInfo(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
enum TvmSvmEvent {
    ReceiveRoundRelays { keys: Vec<UInt256> },
    Confirm { public_key: UInt256 },
    Reject { public_key: UInt256 },
    Rejected,
    Closed,
}

impl ReadFromTransaction for TvmSvmEvent {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        if has_rejected_event(ctx) {
            return Some(Self::Rejected);
        }

        let in_msg = ctx.in_msg;
        let event = match in_msg.header() {
            ton_block::CommonMsgInfo::ExtInMsgInfo(_) => {
                let (public_key, body) = read_external_in_msg(&in_msg.body()?)?;

                match read_function_id(&body) {
                    Ok(id) if id == tvm_svm_event_contract::confirm().input_id => {
                        Some(TvmSvmEvent::Confirm { public_key })
                    }
                    Ok(id) if id == tvm_svm_event_contract::reject().input_id => {
                        Some(TvmSvmEvent::Reject { public_key })
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

                        Some(TvmSvmEvent::ReceiveRoundRelays { keys: items })
                    }
                    _ => None,
                }
            }
            ton_block::CommonMsgInfo::ExtOutMsgInfo(_) => None,
        };

        if event.is_none() {
            let balance = ctx.account_state.account.storage.balance.grams;
            if balance.as_u128() == 0 {
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
                _ => anyhow::Error::msg(format!("SVM RPC error: {err}")),
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

type ConnectorState = Arc<AccountObserver<ConnectorEvent>>;

type DefaultHeaders = (PubkeyHeader, TimeHeader, ExpireHeader);

type ConnectorsMap = FxHashMap<UInt256, ConnectorState>;
type EvmTvmEventConfigurationsMap = FxHashMap<UInt256, EvmTvmEventConfigurationState>;
type TvmEvmEventConfigurationsMap = FxHashMap<UInt256, TvmEvmEventConfigurationState>;
type SvmTvmEventConfigurationsMap = FxHashMap<UInt256, SvmTvmEventConfigurationState>;
type TvmSvmEventConfigurationsMap = FxHashMap<UInt256, TvmSvmEventConfigurationState>;
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
    #[error("Account `{0}` not found")]
    AccountNotFound(String),
    #[cfg(feature = "ton")]
    #[error("Token metadata mismatch")]
    TokenMetadataMismatch,
}
