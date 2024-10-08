use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use nekoton_abi::UnpackAbiPlain;
use parking_lot::Mutex;
use tokio::sync::futures::Notified;
use tokio::sync::mpsc;
use tokio::sync::Notify;
use ton_types::UInt256;

use crate::engine::keystore::*;
use crate::engine::ton_contracts::*;
use crate::engine::ton_subscriber::*;
use crate::engine::EngineContext;
use crate::utils::*;

/// Rounds part of relays logic
pub struct Staking {
    /// Shared engine context
    context: Arc<EngineContext>,

    /// Current relay round info
    current_relay_round: Mutex<CurrentRelayRound>,

    /// Notifier for `RelayRoundInitialized` events
    relay_round_started_notify: Notify,
    /// Notifier for `ElectionStarted` events
    elections_start_notify: Notify,
    /// Notifier for `ElectionEnded` events
    elections_end_notify: Notify,
    /// Notifier for `RelayConfigUpdated` events
    relay_config_updated_notify: Notify,
    /// Notifier for `DepositProcessed` events
    user_data_balance_changed_notify: Notify,

    /// Whether relay participates in current round
    participates_in_round: Tristate,
    /// Whether relay was elected during elections in current round.
    elected: Tristate,

    /// Staking contract address
    staking_account: UInt256,
    /// Staking events listener
    staking_observer: Arc<AccountObserver<(RoundState, StakingEvent)>>,

    /// Staker user data contract address
    user_data_account: UInt256,
    /// UserData events listener
    user_data_observer: Arc<AccountObserver<UserDataEvent>>,
}

impl Staking {
    pub async fn new(ctx: Arc<EngineContext>, staking_account: UInt256) -> Result<Arc<Self>> {
        // Prepare staking
        ctx.ensure_user_data_confirmed(staking_account)
            .await
            .context("Failed to ensure that user data is confirmed")?;

        // Prepare initial data
        let (shard_accounts, relay_round_details, relay_round_state, user_data_account) = loop {
            // Load all shard states
            let shard_accounts = ctx.get_all_shard_accounts().await?;

            // Get all info from staking contract
            let staking_contract = shard_accounts
                .find_account(&staking_account)?
                .context("Staking contract not found")?;
            let staking_contract = StakingContract(&staking_contract);

            let relay_round_state = staking_contract
                .get_round_state()
                .context("Current relay round not found")?;

            let relay_round_address = staking_contract
                .get_relay_round_address(relay_round_state.number)
                .context("Failed to get current relay round address")?;

            // Get all info from current relay round contract
            let relay_round_details = match shard_accounts.find_account(&relay_round_address)? {
                Some(contract) => RelayRoundContract(&contract)
                    .get_details()
                    .context("Failed to get relay round details")?,
                None => {
                    // Wait for the next state
                    tracing::warn!("current relay round contract not found");
                    continue;
                }
            };

            if !relay_round_details.relays_installed {
                // Wait for the next state
                tracing::warn!("relay round was not initialized yet");
                continue;
            }

            let user_data_account = staking_contract
                .get_user_data_address(&ctx.staker_account)
                .context("User data account not found")?;

            break (
                shard_accounts,
                relay_round_details,
                relay_round_state,
                user_data_account,
            );
        };

        let participates_in_round = relay_round_details
            .staker_addrs
            .contains(&ctx.staker_account);

        let user_data_contract = shard_accounts
            .find_account(&user_data_account)?
            .context("User data account not found")?;
        let user_data_balance = UserDataContract(&user_data_contract)
            .get_details()
            .context("Failed to get user data details")?
            .token_balance;

        let check_elected = |elections_account_address: &UInt256| -> anyhow::Result<bool> {
            let elections_contract = shard_accounts
                .find_account(elections_account_address)?
                .context("Next elections contract not found")?;
            let elections_contract = ElectionsContract(&elections_contract);
            Ok(elections_contract
                .staker_addrs()
                .context("Failed to get staker addresses from next elections contract")?
                .contains(&ctx.staker_account))
        };

        let (should_vote, elected) = match &relay_round_state.elections_state {
            ElectionsState::NotStarted { .. } => (false, None),
            ElectionsState::Started { .. } => {
                let elected = check_elected(&relay_round_state.next_elections_account)?;
                (!ctx.settings.ignore_elections && !elected, Some(elected))
            }
            ElectionsState::Finished => {
                let elected = check_elected(&relay_round_state.next_elections_account)?;
                (false, Some(elected))
            }
        };

        let (staking_events_tx, staking_events_rx) = mpsc::unbounded_channel();
        let (user_data_events_tx, user_data_events_rx) = mpsc::unbounded_channel();

        // Create object
        let staking = Arc::new(Self {
            context: ctx,
            current_relay_round: Mutex::new(CurrentRelayRound {
                user_data_balance,
                state: relay_round_state,
            }),
            relay_round_started_notify: Default::default(),
            elections_start_notify: Default::default(),
            elections_end_notify: Default::default(),
            relay_config_updated_notify: Default::default(),
            user_data_balance_changed_notify: Default::default(),
            participates_in_round: Tristate::new(Some(participates_in_round)),
            elected: Tristate::new(elected),
            staking_account,
            staking_observer: AccountObserver::new(&staking_events_tx),
            user_data_account,
            user_data_observer: AccountObserver::new(&user_data_events_tx),
        });

        let staking_clone = staking.clone();
        let elections_end_fut = staking_clone.elections_end_notify.notified();

        start_listening_events(
            &staking,
            "StakingContract",
            staking_events_rx,
            Self::process_staking_event,
        );
        start_listening_events(
            &staking,
            "UserDataContract",
            user_data_events_rx,
            Self::process_user_data_event,
        );

        // Subscribe observers
        let context = &staking.context;
        context
            .ton_subscriber
            .add_transactions_subscription([staking_account], &staking.staking_observer);
        context
            .ton_subscriber
            .add_transactions_subscription([user_data_account], &staking.user_data_observer);

        if should_vote {
            tokio::select! {
                _ = staking_clone.become_relay_next_round() => {}
                _ = elections_end_fut => {
                    tracing::info!("elections finished");
                }
            }
        }

        staking.start_managing_elections();

        staking.collect_all_unclaimed_reward().await?;

        Ok(staking)
    }

    pub fn metrics(&self) -> StakingMetrics {
        let current_relay_round = self.current_relay_round.lock();

        StakingMetrics {
            current_relay_round: current_relay_round.state.number,
            user_data_tokens_balance: current_relay_round.user_data_balance,
            elections_state: current_relay_round.state.elections_state,
            ignore_elections: self.context.settings.ignore_elections,
            participates_in_round: self.participates_in_round.load(),
            elected: self.elected.load(),
        }
    }

    #[tracing::instrument(skip(self))]
    async fn collect_all_unclaimed_reward(self: &Arc<Self>) -> Result<()> {
        tracing::info!("searching for the staking account");
        let shard_accounts = self.context.get_all_shard_accounts().await?;
        let staking_contract = shard_accounts
            .find_account(&self.staking_account)?
            .context("Staking contract not found")?;
        let staking_contract = StakingContract(&staking_contract);

        let try_collect_reward = |relay_round: u32| -> Result<()> {
            let relay_round_address = staking_contract
                .get_relay_round_address(relay_round)
                .context("Failed to compute relay round address")?;

            let relay_round_contract = match shard_accounts.find_account(&relay_round_address) {
                Ok(Some(contract)) => contract,
                Ok(None) => {
                    tracing::warn!(relay_round, "relay round not found");
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!(relay_round, "failed to find relay round: {e:?}");
                    return Ok(());
                }
            };
            let relay_round_contract = RelayRoundContract(&relay_round_contract);

            // Check if staker has unclaimed reward
            match relay_round_contract.has_unclaimed_reward(self.context.staker_account) {
                Ok(true) => { /* continue */ }
                Ok(false) => return Ok(()),
                Err(e) => {
                    tracing::warn!(relay_round, "failed to check unclaimed reward: {e:?}");
                }
            };

            // Get end time and collect reward
            match relay_round_contract.end_time() {
                Ok(end_time) => {
                    let staking = self.clone();
                    tokio::spawn(async move {
                        if let Err(e) = staking
                            .get_reward_for_relay_round(relay_round, end_time)
                            .await
                        {
                            tracing::error!(relay_round, "failed to collect reward: {e:?}");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!(relay_round, "failed to check round end time: {e:?}");
                }
            };
            Ok(())
        };

        let current_round = staking_contract
            .get_relay_rounds_details()
            .context("Failed to get relay rounds details")?
            .current_relay_round;
        let oldest_relay_round = current_round.saturating_sub(2);
        tracing::info!(current_round, oldest_relay_round);

        for relay_round in (oldest_relay_round..=current_round).rev() {
            tracing::info!(relay_round, "collecting reward for an old relay round");
            try_collect_reward(relay_round)?;
        }

        Ok(())
    }

    async fn process_staking_event(
        self: Arc<Self>,
        (_, (round_state, event)): (UInt256, (RoundState, StakingEvent)),
    ) -> Result<()> {
        // Lock round state before notification.
        // NOTE: at this time it can also be locked in elections management loop
        let mut current_relay_round = self.current_relay_round.lock();
        current_relay_round.state = round_state;

        // Reset `elected` flag if elections were not started yet
        if matches!(
            &current_relay_round.state.elections_state,
            ElectionsState::NotStarted { .. }
        ) {
            self.elected.store(None);
        }

        match event {
            StakingEvent::ElectionStarted(_) => {
                self.elections_start_notify.notify_waiters();

                // Set `elected` as `false` on each election start
                self.elected.store_if_empty(false);

                // Do nothing if elections are ignored
                if self.context.settings.ignore_elections {
                    return Ok(());
                }

                // Start participating in elections
                let staking = self.clone();
                tokio::spawn(async move {
                    let notify_fut = staking.elections_end_notify.notified();
                    tokio::select! {
                        result = staking.become_relay_next_round() => {
                            if let Err(e) = result {
                                tracing::error!("failed to become relay in the next round: {e:?}");
                            }
                        },
                        _ = notify_fut => {
                            tracing::warn!("early exit from become_relay_next_round due to the elections end");
                        }
                    }
                });
            }
            StakingEvent::ElectionEnded(_) => {
                self.elections_end_notify.notify_waiters();
            }
            StakingEvent::RelayRoundInitialized(event) => {
                self.relay_round_started_notify.notify_waiters();

                // Reset `participates_in_round` flag on each round start
                self.participates_in_round.store(None);

                // Spawn participation status checker
                let staking = self.clone();
                tokio::spawn(async move {
                    if let Err(e) = staking.update_participates_in_round_status().await {
                        tracing::error!("failed to update `participates_in_round` flag: {e:?}");
                    }
                });

                // Spawn delayed reward collection
                let staking = self.clone();
                tokio::spawn(async move {
                    // Collect reward
                    if let Err(e) = staking
                        .get_reward_for_relay_round(event.round_num, event.round_end_time)
                        .await
                    {
                        tracing::error!(
                            relay_round = event.round_num,
                            "failed to collect reward: {e:?}",
                        );
                    }
                });
            }
            StakingEvent::RelayConfigUpdated(_) => {
                self.relay_config_updated_notify.notify_waiters();
            }
        }

        // NOTE: `current_relay_round` lock is dropped here
        Ok(())
    }

    async fn process_user_data_event(
        self: Arc<Self>,
        (_, event): (UInt256, UserDataEvent),
    ) -> Result<()> {
        let keystore = &self.context.keystore;

        match event {
            UserDataEvent::RelayKeysUpdated(event) => {
                if event.ton_pubkey != keystore.ton.public_key()
                    || &event.eth_address != keystore.eth.address().as_fixed_bytes()
                {
                    tracing::error!(
                        "FATAL ERROR. Staker sent different keys. Current relay setup is not operational now"
                    );
                    self.context.shutdown_requests_tx.send(())?;
                }
            }
            UserDataEvent::RelayMembershipRequested(event) => {
                tracing::info!(
                    ton_pubkey = event.ton_pubkey.to_hex_string(),
                    eth_address = hex::encode(event.eth_address),
                    "relay membership requested",
                );
                self.elected.store(Some(true));
            }
            UserDataEvent::DepositProcessed(event) => {
                let mut current_relay_round = self.current_relay_round.lock();
                current_relay_round.user_data_balance = event.new_balance;
                self.user_data_balance_changed_notify.notify_waiters();
            }
            _ => { /* ignore */ }
        }

        Ok(())
    }

    fn start_managing_elections(self: &Arc<Self>) {
        let staking = Arc::downgrade(self);

        tokio::spawn(async move {
            loop {
                // Get staking if it is still alive
                let staking = match staking.upgrade() {
                    Some(staking) => staking,
                    None => return,
                };

                // Prepare notification futures
                let (elections_state, relay_config_updated_fut) = {
                    // Acquire mutex lock here
                    let current_relay_round = staking.current_relay_round.lock();

                    // Prepare pending elections state
                    let elections_state = current_relay_round.state.elections_state;
                    tracing::info!(?elections_state, "elections management loop");

                    let elections_state = match elections_state {
                        ElectionsState::NotStarted { start_time } => {
                            PendingElectionsState::NotStarted {
                                start_time,
                                inner_fut: staking.elections_start_notify.notified(),
                                outer_fut: staking.elections_start_notify.notified(),
                            }
                        }
                        ElectionsState::Started { end_time, .. } => {
                            PendingElectionsState::Started {
                                end_time,
                                inner_fut: staking.elections_end_notify.notified(),
                                outer_fut: staking.elections_end_notify.notified(),
                            }
                        }
                        ElectionsState::Finished => PendingElectionsState::Finished {
                            new_round_fut: staking.relay_round_started_notify.notified(),
                        },
                    };
                    let relay_config_updated_fut = staking.relay_config_updated_notify.notified();

                    // NOTE: `current_relay_round` lock is dropped here, so it is guaranteed that
                    // no other events are executed in same time
                    (elections_state, relay_config_updated_fut)
                };

                let now = chrono::Utc::now().timestamp() as u64;

                // Process election state
                match elections_state {
                    PendingElectionsState::NotStarted {
                        start_time,
                        inner_fut,
                        outer_fut,
                    } => {
                        let staking = staking.clone();

                        let action = async move {
                            let delay = (start_time as u64).saturating_sub(now);

                            // Wait elections start time
                            tracing::info!(
                                now,
                                delay_sec = delay,
                                "waiting for elections to start"
                            );
                            tokio::time::sleep(Duration::from_secs(delay)).await;

                            // Start elections
                            tracing::info!("starting elections");
                            if let Err(e) = staking.start_election().await {
                                tracing::error!("failed to start election: {e:?}");
                            }

                            // Wait actual elections start
                            tracing::info!("waiting after the start of the elections");
                            inner_fut.await;
                        };

                        tokio::select! {
                            _ = action => continue,
                            _ = outer_fut => {
                                tracing::warn!("elections loop: cancelling elections start, already started");
                            }
                            _ = relay_config_updated_fut => {
                                tracing::warn!("elections loop: cancelling elections start, timings changed");
                            }
                        }
                    }
                    PendingElectionsState::Started {
                        end_time,
                        inner_fut,
                        outer_fut,
                    } => {
                        let staking = staking.clone();
                        let action = async move {
                            let delay = (end_time as u64).saturating_sub(now);

                            // Wait elections end time
                            tracing::info!(
                                now,
                                delay_sec = delay,
                                "waiting for elections to finish"
                            );
                            tokio::time::sleep(Duration::from_secs(delay)).await;

                            // End elections
                            tracing::info!("ending elections");
                            if let Err(e) = staking.end_election().await {
                                tracing::error!("failed to end election: {e:?}");
                            }

                            // Wait actual elections end
                            tracing::info!("waiting after the end of the elections");
                            inner_fut.await;
                        };

                        tokio::select! {
                            _ = action => continue,
                            _ = outer_fut => {
                                tracing::warn!("elections loop: cancelling elections ending, already ended");
                            }
                            _ = relay_config_updated_fut => {
                                tracing::warn!("elections loop: cancelling elections ending, timings changed");
                            }
                        }
                    }
                    PendingElectionsState::Finished { new_round_fut } => {
                        // Wait new round initialization
                        tracing::info!("elections loop: waiting for a new round");
                        new_round_fut.await
                    }
                }
            }
        });
    }

    /// Delivers `becomeRelayNextRound` message to user data contract
    async fn become_relay_next_round(&self) -> Result<()> {
        // Wait until user has enough balance to be elected
        loop {
            let (user_data_balance_changed_fut, relay_config_updated_fut) = {
                let current_relay_round = self.current_relay_round.lock();

                let user_data_balance = current_relay_round.user_data_balance;
                let min_relay_deposit = current_relay_round.state.min_relay_deposit;

                // Check if user can be elected
                if user_data_balance >= min_relay_deposit {
                    tracing::info!(
                        user_data_balance,
                        min_relay_deposit,
                        "user has enough balance to be elected",
                    );
                    break;
                }

                // Wait some changes
                tracing::info!(
                    user_data_balance,
                    min_relay_deposit,
                    "user doesn't have enough balance to be elected",
                );

                // User data notifications
                (
                    self.user_data_balance_changed_notify.notified(),
                    self.relay_config_updated_notify.notified(),
                )
            };

            // Wait until balance or config are changed
            tokio::select! {
                _ = user_data_balance_changed_fut => {
                    tracing::info!("next round procedure loop: user data balance changed");
                }
                _ = relay_config_updated_fut => {
                    tracing::info!("next round procedure loop: relay config updated")
                }
            }
        }

        // Send message `becomeRelayNextRound`
        self.context
            .deliver_message(
                self.user_data_observer.clone(),
                UnsignedMessage::new(
                    user_data_contract::become_relay_next_round(),
                    self.user_data_account,
                ),
                // Condition is always true because this method should
                // always be called inside `tokio::select`
                || true,
            )
            .await
    }

    /// Delivers `getRewardForRelayRound` message to user data contract at specified time
    async fn get_reward_for_relay_round(&self, relay_round: u32, end_time: u32) -> Result<()> {
        const ROUND_OFFSET: u64 = 10; // seconds

        // Wait until round end
        let now = chrono::Utc::now().timestamp() as u64;
        tokio::time::sleep(Duration::from_secs(
            (end_time as u64).saturating_sub(now) + ROUND_OFFSET,
        ))
        .await;

        // Collect
        self.context
            .deliver_message(
                self.user_data_observer.clone(),
                UnsignedMessage::new(
                    user_data_contract::get_reward_for_relay_round(),
                    self.user_data_account,
                )
                .arg(relay_round),
                // Condition is always true because this method is always accepted by the contract.
                // (It is an exception situation otherwise)
                || true,
            )
            .await
    }

    /// Delivers `startElectionOnNewRound` message to staking contract
    async fn start_election(&self) -> Result<()> {
        self.context
            .deliver_message(
                self.staking_observer.clone(),
                UnsignedMessage::new(
                    staking_contract::start_election_on_new_round(),
                    self.staking_account,
                ),
                // Condition is always true because this method should
                // always be called inside `tokio::select`
                || true,
            )
            .await
    }

    /// Delivers `endElection` message to staking contract
    async fn end_election(&self) -> Result<()> {
        self.context
            .deliver_message(
                self.staking_observer.clone(),
                UnsignedMessage::new(staking_contract::end_election(), self.staking_account),
                // Condition is always true because this method should
                // always be called inside `tokio::select`
                || true,
            )
            .await
    }

    /// Checks whether this relay is in current relay round
    async fn update_participates_in_round_status(&self) -> Result<()> {
        let shard_accounts = self.context.get_all_shard_accounts().await?;
        let staking_contract = shard_accounts
            .find_account(&self.staking_account)?
            .context("Staking contract not found")?;
        let staking_contract = StakingContract(&staking_contract);

        let relay_rounds_details = staking_contract
            .get_relay_rounds_details()
            .context("Failed to get relay_rounds_details")?;

        let relay_round_address = staking_contract
            .get_relay_round_address(relay_rounds_details.current_relay_round)
            .context("Failed to compute relay round address")?;
        let relay_round_contract = shard_accounts
            .find_account(&relay_round_address)?
            .context("Current relay round contract not found")?;
        let relay_round_contract = RelayRoundContract(&relay_round_contract);

        self.participates_in_round.store_if_empty(
            relay_round_contract
                .get_details()
                .context("Failed to get relay round details")?
                .staker_addrs
                .contains(&self.staking_account),
        );

        Ok(())
    }
}

pub struct StakingMetrics {
    pub current_relay_round: u32,
    pub user_data_tokens_balance: u128,
    pub elections_state: ElectionsState,
    pub ignore_elections: bool,
    pub participates_in_round: Option<bool>,
    pub elected: Option<bool>,
}

/// Relay round and user data params
struct CurrentRelayRound {
    user_data_balance: u128,
    state: RoundState,
}

impl EngineContext {
    /// Ensures that TON pubkey and ETH address are confirmed in UserData
    async fn ensure_user_data_confirmed(self: &Arc<Self>, staking_account: UInt256) -> Result<()> {
        let shard_accounts = self.get_all_shard_accounts().await?;
        let staking_contract = shard_accounts
            .find_account(&staking_account)?
            .context("Staking contract not found")?;
        let staking_contract = StakingContract(&staking_contract);

        // Get bridge ETH event configuration
        let bridge_event_configuration =
            staking_contract.get_eth_bridge_configuration_details(&shard_accounts)?;
        tracing::info!(
            ?bridge_event_configuration,
            "found bridge event configuration"
        );

        // Initialize user data
        let user_data_account = staking_contract.get_user_data_address(&self.staker_account)?;
        tracing::info!(account = %DisplayAddr(user_data_account), "found user data account");
        let user_data_contract = shard_accounts
            .find_account(&user_data_account)?
            .context("User data account not found")?;
        let user_data_contract = UserDataContract(&user_data_contract);

        user_data_contract
            .ensure_verified(self, user_data_account, bridge_event_configuration)
            .await
    }
}

impl UserDataContract<'_> {
    /// Ensures that TON pubkey and ETH address are confirmed in UserData
    async fn ensure_verified(
        &self,
        context: &Arc<EngineContext>,
        user_data_account: UInt256,
        bridge_event_configuration: EthTonEventConfigurationDetails,
    ) -> Result<()> {
        let ton_pubkey_confirmed_notify = Arc::new(Notify::new());
        let eth_address_confirmed_notify = Arc::new(Notify::new());

        let ton_notified = ton_pubkey_confirmed_notify.notified();
        let eth_notified = eth_address_confirmed_notify.notified();

        let (user_data_events_tx, mut user_data_events_rx) =
            mpsc::unbounded_channel::<(UInt256, UserDataEvent)>();

        let details = self
            .get_details()
            .context("Failed to get UserData details")?;
        tracing::info!(user_data_details = ?details);

        let relay_eth_address = *context.keystore.eth.address().as_fixed_bytes();
        let relay_ton_pubkey = *context.keystore.ton.public_key();

        if details.relay_eth_address != relay_eth_address {
            return Err(StakingError::UserDataEthAddressMismatch.into());
        }
        if details.relay_ton_pubkey != relay_ton_pubkey {
            return Err(StakingError::UserDataTonPublicKeyMismatch.into());
        }

        let user_data_observer = AccountObserver::new(&user_data_events_tx);

        tokio::spawn({
            let ton_pubkey_confirmed_notify = ton_pubkey_confirmed_notify.clone();
            let eth_address_confirmed_notify = eth_address_confirmed_notify.clone();

            async move {
                while let Some((_, event)) = user_data_events_rx.recv().await {
                    match event {
                        UserDataEvent::RelayKeysUpdated(event) => {
                            if event.ton_pubkey != relay_ton_pubkey
                                || event.eth_address != relay_eth_address
                            {
                                tracing::error!(
                                    "TON pubkey or ETH address changed. Relay in current setup may freeze"
                                );
                            }
                        }
                        UserDataEvent::TonPubkeyConfirmed(event) => {
                            if event.ton_pubkey == relay_ton_pubkey {
                                tracing::info!("received TON pubkey confirmation");
                                ton_pubkey_confirmed_notify.notify_waiters();
                            } else {
                                tracing::error!(
                                    relay_ton_pubkey = relay_ton_pubkey.to_hex_string(),
                                    event_ton_pubkey = event.ton_pubkey.to_hex_string(),
                                    "confirmed TON pubkey mismatch",
                                );
                            }
                        }
                        UserDataEvent::EthAddressConfirmed(event) => {
                            if event.eth_addr == relay_eth_address {
                                tracing::info!("received ETH address confirmation");
                                eth_address_confirmed_notify.notify_waiters();
                            } else {
                                tracing::error!(
                                    relay_eth_address = hex::encode(relay_eth_address),
                                    event_eth_address = hex::encode(event.eth_addr),
                                    "confirmed ETH address mismatch"
                                );
                            }
                        }
                        UserDataEvent::RelayMembershipRequested(_) => { /* ignore */ }
                        UserDataEvent::DepositProcessed(_) => { /* ignore */ }
                    }
                }
            }
        });

        context
            .ton_subscriber
            .add_transactions_subscription([user_data_account], &user_data_observer);

        if details.ton_pubkey_confirmed {
            ton_pubkey_confirmed_notify.notify_waiters();
        } else {
            context
                .deliver_message(
                    user_data_observer.clone(),
                    UnsignedMessage::new(
                        user_data_contract::confirm_ton_account(),
                        user_data_account,
                    ),
                    // Condition is always true because this method is always accepted by the contract
                    || true,
                )
                .await
                .context("Failed confirming TON public key")?;
            tracing::info!("sent TON public key confirmation");
        }

        if details.eth_address_confirmed {
            eth_address_confirmed_notify.notify_waiters();
        } else {
            let subscriber = context
                .eth_subscribers
                .get_subscriber(bridge_event_configuration.network_configuration.chain_id)
                .ok_or(StakingError::RequiredEthNetworkNotFound)?;
            subscriber
                .verify_relay_staker_address(
                    &context.settings.address_verification,
                    context.keystore.eth.secret_key(),
                    context.keystore.eth.address(),
                    context.staker_account,
                    &bridge_event_configuration
                        .network_configuration
                        .event_emitter
                        .into(),
                )
                .await
                .context("Failed confirming ETH address")?;
            tracing::info!("sent ETH address confirmation")
        }

        tracing::info!("waiting for confirmation");
        futures_util::future::join(ton_notified, eth_notified).await;

        Ok(())
    }
}

impl<'a> StakingContract<'a> {
    /// Find bridge ETH event configuration
    fn get_eth_bridge_configuration_details(
        &self,
        shard_accounts: &ShardAccountsMap,
    ) -> Result<EthTonEventConfigurationDetails> {
        let details = self
            .get_details()
            .context("Failed to get staking details")?;
        let configuration_contract = shard_accounts
            .find_account(&details.bridge_event_config_eth_ton)?
            .context("Bridge ETH event configuration not found")?;

        EthTonEventConfigurationContract(&configuration_contract)
            .get_details()
            .context("Failed to get ETH bridge configuration details")
    }

    /// Collect relay round state
    fn get_round_state(&self) -> Result<RoundState> {
        let relay_config = self
            .get_relay_config()
            .context("Failed to get relay config")?;

        let relay_rounds_details = self
            .get_relay_rounds_details()
            .context("Failed to get relay_rounds_details")?;
        tracing::info!(?relay_rounds_details);

        let next_elections_account = self
            .get_election_address(relay_rounds_details.current_relay_round + 1)
            .context("Failed to get next election address")?;
        tracing::info!(next_elections_account = next_elections_account.to_hex_string());

        let elections_state = match relay_rounds_details.current_election_start_time {
            0 if relay_rounds_details.current_election_ended => {
                tracing::info!("elections were already finished");
                ElectionsState::Finished
            }
            0 => {
                tracing::info!("elections were not started yet");
                ElectionsState::NotStarted {
                    start_time: relay_rounds_details.current_relay_round_start_time
                        + relay_config.time_before_election,
                }
            }
            start_time => {
                tracing::info!("elections already started");
                ElectionsState::Started {
                    start_time,
                    end_time: start_time + relay_config.election_time,
                }
            }
        };

        Ok(RoundState {
            number: relay_rounds_details.current_relay_round,
            elections_state,
            next_elections_account,
            min_relay_deposit: relay_config.min_relay_deposit,
        })
    }
}

#[derive(Debug, Clone)]
struct RoundState {
    number: u32,
    elections_state: ElectionsState,
    next_elections_account: UInt256,
    min_relay_deposit: u128,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ElectionsState {
    NotStarted { start_time: u32 },
    Started { start_time: u32, end_time: u32 },
    Finished,
}

enum PendingElectionsState<'a> {
    NotStarted {
        start_time: u32,
        inner_fut: Notified<'a>,
        outer_fut: Notified<'a>,
    },
    Started {
        end_time: u32,
        inner_fut: Notified<'a>,
        outer_fut: Notified<'a>,
    },
    Finished {
        new_round_fut: Notified<'a>,
    },
}

macro_rules! parse_tokens {
    ($res:expr,$fun:expr, $body:expr, $matched:expr) => {
        match $fun
            .decode_input($body)
            .and_then(|tokens| tokens.unpack().map_err(anyhow::Error::from))
        {
            Ok(parsed) => $res = Some($matched(parsed)),
            Err(e) => {
                tracing::error!("failed to parse staking event: {e:?}");
            }
        }
    };
}

#[derive(Debug)]
#[allow(dead_code)]
enum StakingEvent {
    ElectionStarted(ElectionStartedEvent),
    ElectionEnded(ElectionEndedEvent),
    RelayRoundInitialized(RelayRoundInitializedEvent),
    RelayConfigUpdated(RelayConfigUpdatedEvent),
}

impl ReadFromTransaction for (RoundState, StakingEvent) {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        let start = staking_contract::events::election_started();
        let end = staking_contract::events::election_ended();
        let round_init = staking_contract::events::relay_round_initialized();
        let config_updated = staking_contract::events::relay_config_updated();

        let mut res = None;
        ctx.iterate_events(|id, body| {
            if id == start.id {
                parse_tokens!(res, start, body, StakingEvent::ElectionStarted);
            } else if id == end.id {
                parse_tokens!(res, end, body, StakingEvent::ElectionEnded);
            } else if id == round_init.id {
                parse_tokens!(res, round_init, body, StakingEvent::RelayRoundInitialized);
            } else if id == config_updated.id {
                parse_tokens!(res, config_updated, body, StakingEvent::RelayConfigUpdated);
            }
        });
        let res = res?;

        let contract = match ctx.get_account_state() {
            Ok(contract) => contract,
            Err(e) => {
                tracing::error!("failed to find account state after transaction: {e:?}");
                return None;
            }
        };

        match StakingContract(&contract).get_round_state() {
            Ok(state) => Some((state, res)),
            Err(e) => {
                tracing::error!("failed to get round state: {e:?}");
                None
            }
        }
    }
}

#[derive(Debug)]
enum UserDataEvent {
    RelayKeysUpdated(RelayKeysUpdatedEvent),
    TonPubkeyConfirmed(TonPubkeyConfirmedEvent),
    EthAddressConfirmed(EthAddressConfirmedEvent),
    RelayMembershipRequested(RelayMembershipRequestedEvent),
    DepositProcessed(DepositProcessedEvent),
}

impl ReadFromTransaction for UserDataEvent {
    fn read_from_transaction(ctx: &TxContext<'_>) -> Option<Self> {
        let keys_updated = user_data_contract::events::relay_keys_updated();
        let ton_confirmed = user_data_contract::events::ton_pubkey_confirmed();
        let eth_confirmed = user_data_contract::events::eth_address_confirmed();
        let membership_requested = user_data_contract::events::relay_membership_requested();
        let deposit = user_data_contract::events::deposit_processed();

        let mut res = None;
        ctx.iterate_events(|id, body| {
            if id == keys_updated.id {
                parse_tokens!(res, keys_updated, body, UserDataEvent::RelayKeysUpdated)
            } else if id == ton_confirmed.id {
                parse_tokens!(res, ton_confirmed, body, UserDataEvent::TonPubkeyConfirmed)
            } else if id == eth_confirmed.id {
                parse_tokens!(res, eth_confirmed, body, UserDataEvent::EthAddressConfirmed)
            } else if id == membership_requested.id {
                parse_tokens!(
                    res,
                    membership_requested,
                    body,
                    UserDataEvent::RelayMembershipRequested
                )
            } else if id == deposit.id {
                parse_tokens!(res, deposit, body, UserDataEvent::DepositProcessed)
            }
        });
        res
    }
}

#[derive(thiserror::Error, Debug)]
enum StakingError {
    #[error("Required ETH network not found")]
    RequiredEthNetworkNotFound,
    #[error("UserData ETH address mismatch")]
    UserDataEthAddressMismatch,
    #[error("UserData TON public key mismatch")]
    UserDataTonPublicKeyMismatch,
}
