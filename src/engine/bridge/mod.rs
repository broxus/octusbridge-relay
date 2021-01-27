use std::collections::hash_map::Entry;
use std::ops::Deref;

use relay_eth::ws::{EthListener, SyncedHeight};
use relay_ton::contracts::*;

use crate::config::RelayConfig;
use crate::crypto::key_managment::*;
use crate::db::*;
use crate::models::*;
use crate::prelude::*;

use self::eth_events_handler::*;
use self::event_transport::*;
use self::semaphore::*;
use self::ton_events_handler::*;

mod eth_events_handler;
mod event_transport;
mod semaphore;
mod ton_events_handler;

mod utils;

pub async fn make_bridge(
    db: Db,
    configs: RelayConfig,
    key_data: KeyData,
) -> Result<Arc<Bridge>, Error> {
    let ton_transport = configs.ton_settings.transport.make_transport().await?;

    let ton_contract_address =
        MsgAddressInt::from_str(&*configs.ton_settings.bridge_contract_address.0)
            .map_err(|e| Error::msg(e.to_string()))?;

    let relay_contract_address =
        MsgAddressInt::from_str(&*configs.ton_settings.relay_contract_address.0)
            .map_err(|e| Error::msg(e.to_string()))
            .and_then(|address| match address {
                MsgAddressInt::AddrStd(addr) => Ok(addr),
                MsgAddressInt::AddrVar(_) => Err(anyhow!("Unsupported relay address")),
            })?;

    let (bridge_contract, bridge_contract_events) =
        make_bridge_contract(ton_transport.clone(), ton_contract_address).await?;

    let relay_contract = make_relay_contract(
        ton_transport.clone(),
        relay_contract_address,
        key_data.ton.keypair(),
        bridge_contract,
    )
    .await?;

    let eth_listener = Arc::new(
        EthListener::new(
            Url::parse(&configs.eth_settings.node_address)
                .map_err(|e| Error::new(e).context("Bad url for eth_config provided"))?,
            db.clone(),
            configs.eth_settings.tcp_connection_count,
            configs.eth_settings.get_eth_data_timeout,
            configs.eth_settings.get_eth_data_attempts,
            configs.eth_settings.eth_poll_interval,
            configs.eth_settings.eth_poll_attempts,
            configs.eth_settings.bridge_address,
        )
        .await?,
    );

    let eth_signer = key_data.eth.clone();
    let eth_verification_queue = EthVerificationQueue::new(&db)?;
    let scanning_state = ScanningState::new(&db)?;

    let ton = Arc::new(
        EventTransport::new(
            &db,
            ton_transport.clone(),
            scanning_state.clone(),
            relay_contract.clone(),
            configs.ton_settings.clone(),
        )
        .await?,
    );
    let eth = Arc::new(
        EventTransport::new(
            &db,
            ton_transport.clone(),
            scanning_state.clone(),
            relay_contract.clone(),
            configs.ton_settings.clone(),
        )
        .await?,
    );

    let bridge = Arc::new(Bridge {
        db,
        eth_listener,
        relay_contract,
        eth_signer,
        eth_verification_queue,
        configs_state: Arc::new(Default::default()),
        ton,
        eth,
        eth_event_handlers: Arc::new(Default::default()),
        ton_event_handlers: Arc::new(Default::default()),
        configs,
    });

    tokio::spawn({
        let bridge = bridge.clone();
        async move { bridge.run(bridge_contract_events).await }
    });

    Ok(bridge)
}

pub struct Bridge {
    db: Db,
    configs: RelayConfig,
    eth_listener: Arc<EthListener>,

    relay_contract: Arc<RelayContract>,

    eth_signer: EthSigner,
    eth_verification_queue: EthVerificationQueue,
    configs_state: Arc<RwLock<ConfigsState>>,

    ton: Arc<EventTransport<TonEventConfigurationContract>>,
    eth: Arc<EventTransport<EthEventConfigurationContract>>,

    eth_event_handlers: Arc<EventsHandlerMap<EthEventsHandler>>,
    ton_event_handlers: Arc<EventsHandlerMap<TonEventsHandler>>,
}

type EventsHandlerMap<T> = RwLock<HashMap<u64, Arc<T>>>;

impl Bridge {
    async fn run<T>(self: Arc<Self>, mut bridge_contract_events: T) -> Result<(), Error>
    where
        T: Stream<Item = BridgeContractEvent> + Send + Unpin + 'static,
    {
        log::info!(
            "Bridge started. Relay account: {}",
            self.relay_contract.address()
        );

        // Subscribe to bridge events
        tokio::spawn({
            let bridge = self.clone();

            async move {
                while let Some(event) = bridge_contract_events.next().await {
                    match event {
                        BridgeContractEvent::EventConfigurationCreationEnd {
                            id,
                            address,
                            active: true,
                            event_type: EventType::ETH,
                        } => {
                            let bridge = bridge.clone();
                            tokio::spawn(async move {
                                bridge
                                    .subscribe_to_eth_events_configuration(id, address, None)
                                    .await
                            });
                        }
                        BridgeContractEvent::EventConfigurationCreationEnd {
                            id,
                            address,
                            active: true,
                            event_type: EventType::TON,
                        } => {
                            let bridge = bridge.clone();
                            tokio::spawn(async move {
                                bridge
                                    .subscribe_to_ton_events_configuration(id, address)
                                    .await
                            });
                        }
                        BridgeContractEvent::EventConfigurationUpdateEnd {
                            id,
                            active,
                            address,
                            event_type,
                        } => {
                            let bridge = bridge.clone();
                            tokio::spawn(async move {
                                match event_type {
                                    EventType::ETH => {
                                        bridge.unsubscribe_from_eth_events_configuration(id).await;
                                    }
                                    EventType::TON => {
                                        bridge.unsubscribe_from_ton_events_configuration(id).await;
                                    }
                                };

                                if active {
                                    match event_type {
                                        EventType::ETH => {
                                            bridge
                                                .subscribe_to_eth_events_configuration(
                                                    id, address, None,
                                                )
                                                .await
                                        }
                                        EventType::TON => {
                                            bridge
                                                .subscribe_to_ton_events_configuration(id, address)
                                                .await
                                        }
                                    }
                                }
                            });
                        }
                        _ => {
                            // TODO: handle other events
                        }
                    }
                }
            }
        });

        // Get all configs before now
        let known_contracts = self
            .relay_contract
            .bridge()
            .get_active_event_configurations()
            .await
            .expect("Failed to get known event configurations"); // TODO: is it really a fatal error?

        // Wait for all existing configuration contracts subscriptions to start ETH part properly
        let semaphore = Semaphore::new(known_contracts.len());
        for active_configuration in known_contracts.into_iter() {
            let bridge = self.clone();
            let semaphore = semaphore.clone();
            tokio::spawn(async move {
                match active_configuration.event_type {
                    EventType::ETH => {
                        bridge
                            .subscribe_to_eth_events_configuration(
                                active_configuration.id,
                                active_configuration.address,
                                Some(semaphore),
                            )
                            .await
                    }
                    EventType::TON => {
                        bridge
                            .subscribe_to_ton_events_configuration(
                                active_configuration.id,
                                active_configuration.address,
                            )
                            .await;
                        semaphore.notify().await;
                    }
                }
            });
        }

        // Wait until all initial subscriptions done
        log::trace!("waiting semaphore");
        semaphore.wait().await;
        log::trace!("semaphore done");

        // Restart sending for all enqueued confirmations
        self.eth.retry_pending();

        // Subscribe for ETH blocks and events
        let mut eth_events_rx = self.eth_listener.start().await?;

        // Spawn pending confirmations queue processing
        tokio::spawn(self.clone().watch_pending_confirmations());

        // Enqueue new events from ETH
        while let Some(event) = eth_events_rx.next().await {
            let event: relay_eth::ws::Event = match event {
                Ok(event) => event,
                Err(e) => {
                    log::error!("Failed parsing data from ethereum stream: {:?}", e);
                    continue;
                }
            };

            tokio::spawn(self.clone().process_eth_event(event));
        }

        // Done
        Ok(())
    }

    ///Sets eth height
    pub async fn change_eth_height(&self, height: u64) -> Result<(), Error> {
        let actual_height = self.eth_listener.get_synced_height().await?.as_u64();
        if actual_height < height {
            return Err(anyhow::anyhow!(
                "Height provided by user is higher, then actual eth height. Cowardly refusing"
            ));
        }
        self.eth_listener.change_eth_height(height)?;
        Ok(())
    }

    /// Restart voting for failed transactions
    pub fn retry_failed(&self) {
        self.eth.retry_failed();
        self.ton.retry_failed();
    }

    pub fn ton_relay_address(&self) -> MsgAddrStd {
        self.relay_contract.address().clone()
    }

    pub fn eth_pubkey(&self) -> secp256k1::PublicKey {
        self.eth_signer.pubkey()
    }

    pub async fn get_event_configurations(
        &self,
    ) -> Result<Vec<(u64, EthEventConfiguration)>, anyhow::Error> {
        let state = self.configs_state.read().await;
        Ok(state.eth_configs_map.values().cloned().collect())
    }

    pub async fn create_event_configuration(
        &self,
        configuration_id: u64,
        address: MsgAddressInt,
        event_type: EventType,
    ) -> Result<(), anyhow::Error> {
        self.relay_contract
            .initialize_event_configuration_creation(configuration_id, &address, event_type)
            .await?;
        Ok(())
    }

    pub async fn vote_for_ethereum_event_configuration(
        &self,
        configuration_id: u64,
        voting: Voting,
    ) -> Result<(), anyhow::Error> {
        self.relay_contract
            .vote_for_event_configuration_creation(configuration_id, voting)
            .await?;
        Ok(())
    }

    async fn check_suspicious_event(self: Arc<Self>, event: EthEventVoteData) {
        async fn check_event(
            configs: &ConfigsState,
            result_to_check: Result<(Address, Vec<u8>), Error>,
            event: &EthEventVoteData,
        ) -> Result<(), Error> {
            let (address, data) = result_to_check?;
            let (_, eth_abi, ton_abi) = if let Some(abi) = configs.address_topic_map.get(&address) {
                abi
            } else {
                return Err(anyhow!(
                    "We have no info about {} to get abi. Rejecting transaction",
                    address
                ));
            };
            let expected_tokens = ethabi::decode(eth_abi, &data).map_err(|e| {
                Error::from(e)
                    .context("Can not verify data, that other relay sent. Assuming it's fake.")
            })?;
            // Decode event data
            let got_tokens: Vec<ethabi::Token> =
                utils::parse_eth_event_data(&eth_abi, &ton_abi, event.event_data.clone())
                    .map_err(|e| e.context("Failed decoding other relay data as eth types"))?;

            if got_tokens == expected_tokens {
                Ok(())
            } else {
                Err(anyhow!(
                    "Decoded tokens are not equal with that other relay sent"
                ))
            }
        }
        let check_result = self
            .eth_listener
            .check_transaction(event.event_transaction, event.event_index)
            .await;

        if let Err(e) = {
            match check_event(
                self.configs_state.read().await.deref(),
                check_result,
                &event,
            )
            .await
            {
                Ok(_) => {
                    log::info!("Confirming transaction. Hash: {}", event.event_transaction);
                    self.eth
                        .enqueue_vote(EventTransaction::Confirm(event))
                        .await
                }
                Err(e) => {
                    log::warn!("Rejection: {:?}", e);
                    self.eth.enqueue_vote(EventTransaction::Reject(event)).await
                }
            }
        } {
            log::error!("Critical error while spawning vote: {:?}", e)
        }
    }

    // Watch ETH votes queue
    async fn watch_pending_confirmations(self: Arc<Self>) {
        log::debug!("Started watch_unsent_eth_ton_transactions");
        loop {
            let synced_block = match self.eth_listener.get_synced_height().await {
                Ok(a) => a,
                Err(e) => {
                    log::error!("CRITICAL error: {}", e);
                    continue;
                }
            };
            log::debug!("New block: {:?}", synced_block);

            let prepared_blocks = self
                .eth_verification_queue
                .range_before(synced_block.as_u64())
                .await;

            for (entry, event) in prepared_blocks {
                let block_number = event.event_block_number;
                log::debug!(
                    "Found unconfirmed data in block {}: {}",
                    block_number,
                    hex::encode(&event.event_transaction)
                );
                tokio::spawn(self.clone().check_suspicious_event(event));
                entry.remove().expect("Fatal db error");
            }

            if let SyncedHeight::Synced(a) = synced_block {
                let bad_blocks = self.eth_verification_queue.range_after(a).await;
                for (entry, event) in bad_blocks {
                    let block_number = event.event_block_number;
                    log::debug!(
                        "Found suspicious data in block {}: {}",
                        block_number,
                        hex::encode(&event.event_transaction)
                    );
                    tokio::spawn(self.clone().check_suspicious_event(event));
                    entry.remove().expect("Fatal db error");
                }
            }

            tokio::time::delay_for(Duration::from_secs(10)).await;
        }
    }

    // Validate event from ETH and vote for it
    async fn process_eth_event(self: Arc<Self>, event: relay_eth::ws::Event) {
        log::info!(
            "Received event from address: {}. Tx hash: {}.",
            &event.address,
            &event.tx_hash
        );

        // Extend event info
        let (configuration_id, ethereum_event_blocks_to_confirm, ton_data) = {
            let state = self.configs_state.read().await;

            // Find suitable event configuration
            let (configuration_id, event_config) = match state.eth_configs_map.get(&event.address) {
                Some(data) => data,
                None => {
                    log::error!("FATAL ERROR. Failed mapping event_configuration with address");
                    return;
                }
            };

            // Decode event data
            let decoded_data: Option<Result<(&[ethabi::ParamType], Vec<ethabi::Token>), _>> = event
                .topics
                .iter()
                .map(|topic_id| state.topic_abi_map.get(topic_id))
                .filter_map(|x| x)
                .map(|x| ethabi::decode(x, &event.data).map(|values| (x.as_slice(), values)))
                // Taking first element, cause topics and abi shouldn't overlap more than once
                .next();

            let (abi, topic_tokens) = match decoded_data {
                Some(a) => match a {
                    Ok(a) => a,
                    Err(e) => {
                        log::error!("Failed decoding data from event: {}", e);
                        return;
                    }
                },
                None => {
                    log::error!("No data from event could be parsed");
                    return;
                }
            };

            let ton_data: Result<Vec<_>, _> = topic_tokens
                .into_iter()
                .zip(abi.iter())
                .into_iter()
                .map(|(eth, abi)| utils::map_eth_to_ton_with_abi(eth, abi))
                .collect();

            let ton_data = match ton_data {
                Ok(data) => data,
                Err(e) => {
                    log::error!("Failed mapping eth event data: {}", e);
                    return;
                }
            };

            (
                *configuration_id,
                event_config
                    .event_blocks_to_confirm
                    .to_u64()
                    .unwrap_or_else(u64::max_value),
                ton_data,
            )
        };

        // Prepare confirmation

        let event_data = match utils::pack_token_values(ton_data) {
            Ok(a) => a,
            Err(e) => {
                log::error!("Failed mapping ton_data to cell: {:?}", e);
                return;
            }
        };

        let prepared_data = EthEventVoteData {
            configuration_id,
            event_transaction: event.tx_hash,
            event_index: event.event_index as u32,
            event_data,
            event_block_number: event.block_number,
            event_block: event.block_hash,
        };

        let target_block_number = event.block_number + ethereum_event_blocks_to_confirm;

        log::info!(
            "Inserting transaction for block {} with queue number: {}",
            event.block_number,
            target_block_number
        );
        self.eth_verification_queue
            .insert(target_block_number, &prepared_data)
            .await
            .expect("Fatal db error");
    }

    /// Creates a listener for TON event votes in TON and its target contract
    async fn subscribe_to_ton_events_configuration(
        self: Arc<Self>,
        configuration_id: u64,
        address: MsgAddressInt,
    ) {
        let verification_queue = match TonVerificationQueue::new(&self.db, configuration_id) {
            Ok(queue) => queue,
            Err(e) => {
                log::error!(
                    "Failed to open verification queue db for configuration {}: {:?}",
                    configuration_id,
                    e
                );
                return;
            }
        };

        let handler = match TonEventsHandler::new(
            self.ton.clone(),
            self.eth_signer.clone(),
            verification_queue,
            configuration_id,
            address,
            &self.configs.ton_settings,
        )
        .await
        {
            Ok(handler) => handler,
            Err(e) => {
                log::error!("Failed to subscribe to TON events configuration: {:?}", e);
                return;
            }
        };

        self.ton_event_handlers
            .write()
            .await
            .insert(configuration_id, handler);
    }

    /// Unsubscribe from TON event configuration
    async fn unsubscribe_from_ton_events_configuration(&self, configuration_id: u64) {
        self.ton_event_handlers
            .write()
            .await
            .remove(&configuration_id);
    }

    /// Creates a listener for ETH event votes in TON
    async fn subscribe_to_eth_events_configuration(
        &self,
        configuration_id: u64,
        address: MsgAddressInt,
        semaphore: Option<Semaphore>,
    ) {
        let handler = match EthEventsHandler::uninit(
            self.eth.clone(),
            self.eth_verification_queue.clone(),
            configuration_id,
            address,
            &self.configs.ton_settings,
        )
        .await
        {
            Ok(handler) => handler,
            Err(e) => {
                log::error!("Failed to subscribe to ETH events configuration: {:?}", e);
                return;
            }
        };

        // Insert configuration and start listening ETH events
        self.subscribe_to_eth_topic(configuration_id, handler.details())
            .await;

        let handler = handler.start().await;

        log::trace!("notifying semaphore: {}", semaphore.is_some());
        semaphore.try_notify().await;

        self.eth_event_handlers
            .write()
            .await
            .insert(configuration_id, handler);
    }

    /// Unsubscribe from ETH event configuration
    async fn unsubscribe_from_eth_events_configuration(&self, configuration_id: u64) {
        let mut eth_events_handlers = self.eth_event_handlers.write().await;
        if let Entry::Occupied(entry) = eth_events_handlers.entry(configuration_id) {
            let eth_event_configuration = entry.remove();
            self.unsubscribe_from_eth_topic(eth_event_configuration.details())
                .await;
        }
    }

    /// Registers topic for specified address in ETH
    async fn subscribe_to_eth_topic(&self, configuration_id: u64, details: &EthEventConfiguration) {
        let mut configs_state = self.configs_state.write().await;
        configs_state.set_configuration(configuration_id, details);

        if let Some((topic, _, _)) = configs_state.address_topic_map.get(&details.event_address) {
            self.eth_listener
                .add_topic(details.event_address, *topic)
                .await;
        }
    }

    /// Removes topic from specified address in ETH
    async fn unsubscribe_from_eth_topic(&self, details: &EthEventConfiguration) {
        let mut configs_state = self.configs_state.write().await;
        configs_state.remove_event_address(&details.event_address);

        self.eth_listener
            .unsubscribe_from_address(&details.event_address)
            .await;
    }
}

#[derive(Debug, Clone, Default)]
pub struct ConfigsState {
    pub eth_addr: HashSet<Address>,
    pub address_topic_map: HashMap<Address, EthTopicItem>,
    pub topic_abi_map: HashMap<H256, Vec<ethabi::ParamType>>,
    pub eth_configs_map: HashMap<Address, (u64, EthEventConfiguration)>,
}

type EthTopicItem = (H256, Vec<ethabi::ParamType>, Vec<ton_abi::ParamType>);

impl ConfigsState {
    fn set_configuration(&mut self, configuration_id: u64, configuration: &EthEventConfiguration) {
        let (topic_hash, eth_abi, ton_abi) =
            match utils::parse_eth_abi(&configuration.common.event_abi) {
                Ok(a) => a,
                Err(e) => {
                    log::error!("Failed parsing abi: {:?}", e);
                    return;
                }
            };

        self.eth_addr.insert(configuration.event_address);
        self.address_topic_map.insert(
            configuration.event_address,
            (topic_hash, eth_abi.clone(), ton_abi),
        );
        self.topic_abi_map.insert(topic_hash, eth_abi);
        self.eth_configs_map.insert(
            configuration.event_address,
            (configuration_id, configuration.clone()),
        );
    }

    fn remove_event_address(&mut self, event_address: &Address) {
        self.eth_addr.remove(event_address);
        self.address_topic_map.remove(event_address);
        self.eth_configs_map.remove(event_address);
    }
}
