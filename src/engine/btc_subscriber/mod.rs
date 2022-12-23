use std::collections::{hash_map, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use bitcoin::hash_types::{BlockHash, Txid};
use bitcoin::{Script, TxIn, TxOut};
use esplora_client::Builder;
use rustc_hash::FxHashMap;
use tokio::sync::{oneshot, Semaphore};
use ton_block::MsgAddressInt;

use crate::config::*;
use crate::engine::bridge::*;
use crate::engine::keystore::*;
use crate::utils::*;

pub struct BtcSubscriber {
    config: BtcConfig,
    rpc_client: Arc<esplora_client::AsyncClient>,
    pool: Arc<Semaphore>,
    pending_events: tokio::sync::Mutex<FxHashMap<Txid, EverBTCPendingEvent>>,
    pending_events_count: AtomicUsize,
    unrecognized_proposals_count: AtomicUsize,
}

impl BtcSubscriber {
    pub async fn new(config: BtcConfig) -> Result<Arc<Self>> {
        let builder = Builder::new(&*config.esplora_url);
        let rpc_client = Arc::new(builder.build_async()?);

        let pool = Arc::new(Semaphore::new(config.pool_size));

        let subscriber = Arc::new(Self {
            config,
            rpc_client,
            pool,
            pending_events: Default::default(),
            pending_events_count: Default::default(),
            unrecognized_proposals_count: Default::default(),
        });

        Ok(subscriber)
    }

    pub fn config(&self) -> &BtcConfig {
        &self.config
    }

    pub fn start(self: &Arc<Self>) {
        let subscriber = Arc::downgrade(self);

        tokio::spawn(async move {
            loop {
                let subscriber = match subscriber.upgrade() {
                    Some(subscriber) => subscriber,
                    None => return,
                };

                if let Err(e) = subscriber.update().await {
                    tracing::error!("error occurred during BTC subscriber update: {e:?}");
                }

                tokio::time::sleep(Duration::from_secs(subscriber.config.poll_interval_sec)).await;
            }
        });
    }

    pub fn metrics(&self) -> BtcSubscriberMetrics {
        BtcSubscriberMetrics {
            unrecognized_proposals_count: self.unrecognized_proposals_count.load(Ordering::Acquire),
        }
    }

    pub async fn verify_ton_btc_event(
        &self,
        event_addr: MsgAddressInt,
        receiver: Script,
        amount: u64,
    ) -> Result<VerificationStatus> {
        !! STOPPED HERE
        let rx = {
            let mut pending_events = self.pending_events.lock().await;

            let (tx, rx) = oneshot::channel();

            let created_at = chrono::Utc::now().timestamp() as u64;

            const INIT_INTERVAL_DELAY_SEC: u32 = 300;

            pending_events.insert(
                account_pubkey,
                EverBTCPendingEvent {
                    event_data,
                    status_tx: Some(tx),
                    created_at,
                    delay: INIT_INTERVAL_DELAY_SEC,
                    time: Default::default(),
                },
            );

            self.pending_events_count
                .store(pending_events.len(), Ordering::Release);

            rx
        };

        let res = rx.await?;
        Ok(res)
    }

    pub async fn verify_btc_ton_event(
        &self,
        transaction_data: BtcTonTransactionData,
        account_data: BtcTonAccountData,
    ) -> Result<VerificationStatus> {
        if let VerificationStatus::NotExists =
            self.verify_btc_ton_transaction(transaction_data).await?
        {
            return Ok(VerificationStatus::NotExists);
        }

        self.verify_btc_ton_account(account_data).await
    }

    pub async fn send_message(
        &self,
        message: Message,
        keystore: &Arc<KeyStore>,
    ) -> Result<(), ClientError> {
        let _ = {
            retry(
                || async {
                    let message = message.clone();
                    self.send_and_confirm_message(message, keystore).await
                },
                generate_btc_rpc_backoff_config(Duration::from_secs(
                    self.config.maximum_failed_responses_time_sec,
                )),
                NetworkType::BTC,
                "send btcana transaction",
            )
            .await?
        };

        Ok(())
    }

    pub async fn is_already_voted(
        &self,
        round_number: u32,
        proposal_pubkey: &Pubkey,
        voter_pubkey: &Pubkey,
    ) -> Result<bool> {
        let relay_round_pubkey = btcana_bridge::round_loader::get_relay_round_address(round_number);
        let relay_round_account = self.get_account(&relay_round_pubkey).await?;
        let relay_round_account_data = match relay_round_account {
            Some(account) => btcana_bridge::round_loader::RelayRound::unpack(account.data())?,
            None => {
                return Err(
                    BtcSubscriberError::InvalidRoundAccount(relay_round_pubkey.to_string()).into(),
                )
            }
        };

        let proposal_account = self.get_account(proposal_pubkey).await?;
        let proposal_data = match proposal_account {
            Some(account) => Proposal::unpack_from_slice(account.data())?,
            None => {
                return Err(
                    BtcSubscriberError::InvalidProposalAccount(proposal_pubkey.to_string()).into(),
                )
            }
        };

        let index = relay_round_account_data
            .relays
            .iter()
            .position(|pubkey| pubkey == voter_pubkey)
            .ok_or(BtcSubscriberError::InvalidRound(round_number))?;

        let vote = proposal_data
            .signers
            .get(index)
            .ok_or(BtcSubscriberError::InvalidVotePosition(index))?;

        Ok(*vote != btcana_bridge::bridge_types::Vote::None)
    }

    async fn get_account(&self, account_pubkey: &Pubkey) -> Result<Option<Account>> {
        let account = {
            retry(
                || self.get_account_with_commitment(account_pubkey, self.config.commitment),
                generate_default_timeout_config(Duration::from_secs(
                    self.config.maximum_failed_responses_time_sec,
                )),
                NetworkType::BTC,
                "get account",
            )
            .await?
        };

        Ok(account)
    }

    async fn healthcheck(&self) -> Result<()> {
        retry(
            || self.get_health(),
            generate_default_timeout_config(Duration::from_secs(
                self.config.maximum_failed_responses_time_sec,
            )),
            NetworkType::BTC,
            "healthcheck",
        )
        .await?;

        Ok(())
    }

    async fn update(&self) -> Result<()> {
        tracing::info!(
            pending_events = self.pending_events_count.load(Ordering::Acquire),
            "updating BTC subscriber",
        );

        let mut accounts_to_check = HashSet::new();

        // Get pending Btcana proposals to check
        let programs_to_subscribe = self.programs_to_subscribe.read().clone();
        for program_pubkey in programs_to_subscribe {
            let mut pending_proposals = self.get_pending_proposals(&program_pubkey).await?;
            tracing::info!(?pending_proposals, "found withdrawal proposals to vote");

            let pending_events = self.pending_events.lock().await;

            let unrecognized_proposals = pending_proposals
                .iter()
                .filter(|account| !pending_events.contains_key(account))
                .count();
            tracing::info!(
                %program_pubkey,
                ?unrecognized_proposals,
                "found unrecognized proposals",
            );

            self.unrecognized_proposals_count
                .store(unrecognized_proposals, Ordering::Release);

            // Get rid of unrecognized proposals
            pending_proposals.retain(|account| pending_events.contains_key(account));

            accounts_to_check.extend(pending_proposals.into_iter().collect::<HashSet<Pubkey>>());
        }

        // Get pending TON events to check
        let time = chrono::Utc::now().timestamp() as u64;

        let mut pending_events = self.pending_events.lock().await;
        for (account, event) in pending_events.iter_mut() {
            if !accounts_to_check.contains(account) && time > event.time {
                tracing::info!(
                    account_pubkey = %account,
                    "adding proposal account from TON->BTC pending events to checklist",
                );

                let time_diff = time - event.created_at;
                match time_diff {
                    // First 5 min
                    0..=300 => event.time = time,
                    // Starting from 5 min until 1 hour, double interval
                    301..=3600 => {
                        event.time = time + event.delay as u64;
                        event.delay *= 2;
                    }
                    // After 1 hour poll using interval from config (Default: 1 hour)
                    _ => event.time = time + self.config.poll_proposals_interval_sec,
                };

                accounts_to_check.insert(*account);
            }
        }

        if !accounts_to_check.is_empty() {
            tracing::info!(?accounts_to_check);
        }

        // Check accounts
        for account_pubkey in accounts_to_check {
            match self.get_account(&account_pubkey).await {
                Ok(Some(account)) => {
                    if let hash_map::Entry::Occupied(mut entry) =
                        pending_events.entry(account_pubkey)
                    {
                        let account_data = match Proposal::unpack_from_slice(account.data()) {
                            Ok(proposal) => proposal,
                            Err(_) => {
                                entry.remove();
                                anyhow::bail!("Failed to unpack {} proposal", account_pubkey);
                            }
                        };

                        let status = entry.get().check(account_data.event);

                        let round_pubkey = btcana_bridge::round_loader::get_relay_round_address(
                            account_data.round_number,
                        );
                        match self.get_account(&round_pubkey).await {
                            Ok(None) => {
                                entry.remove();

                                anyhow::bail!(
                                    "Round {} in btcana doesn't exist",
                                    account_data.round_number
                                );
                            }
                            Err(_) => {
                                // Break handling and repeat later
                                continue;
                            }
                            _ => {
                                // Do nothing and continue handing
                            }
                        }

                        if account_data.is_initialized {
                            if let Some(tx) = entry.get_mut().status_tx.take() {
                                tx.send(status).ok();
                            }

                            entry.remove();
                        }
                    }
                }
                Ok(None) => {
                    tracing::info!(
                        %account_pubkey,
                        "Btcana proposal account doesn't exist yet",
                    );
                }
                Err(e) => {
                    tracing::error!(
                        %account_pubkey,
                        "failed to check btcana proposal: {e:?}",
                    );
                }
            }
        }

        self.pending_events_count
            .store(pending_events.len(), Ordering::Release);

        drop(pending_events);

        Ok(())
    }

    async fn get_pending_proposals(&self, program_pubkey: &Pubkey) -> Result<Vec<Pubkey>> {
        let accounts = {
            retry(
                || async {
                    let mem: Vec<u8> = vec![
                        true as u8,                                               // is_initialized
                        btcana_bridge::bridge_state::AccountKind::Proposal as u8, // account_kind
                        false as u8,                                              // is_executed
                    ];
                    let memcmp = MemcmpEncodedBytes::Base58(bs58::encode(mem).into_string());

                    let config = RpcProgramAccountsConfig {
                        filters: Some(vec![RpcFilterType::Memcmp(Memcmp {
                            offset: 0,
                            bytes: memcmp,
                            encoding: None,
                        })]),
                        account_config: RpcAccountInfoConfig {
                            encoding: Some(UiAccountEncoding::Base64),
                            commitment: Some(self.config.commitment),
                            data_slice: Some(UiDataSliceConfig {
                                offset: 0,
                                length: 0,
                            }),
                        },
                        ..Default::default()
                    };

                    self.get_program_accounts_with_config(program_pubkey, config)
                        .await
                },
                generate_default_timeout_config(Duration::from_secs(
                    self.config.maximum_failed_responses_time_sec,
                )),
                NetworkType::BTC,
                "get program accounts",
            )
            .await?
        };

        Ok(accounts.into_iter().map(|(pubkey, _)| pubkey).collect())
    }

    async fn get_transaction(
        &self,
        tx_id: &Txid,
    ) -> Result<bitcoin::blockdata::transaction::Transaction> {
        let transaction = {
            retry(
                || async { self.get_transaction_async(tx_id).await },
                generate_default_timeout_config(Duration::from_secs(
                    self.config.maximum_failed_responses_time_sec,
                )),
                NetworkType::BTC,
                "get btc transaction",
            )
            .await?
        }
        .ok_or(anyhow::bail!("Transaction {} not found", tx_id.to_string()))?;

        Ok(transaction)
    }

    async fn get_account_with_commitment(
        &self,
        account_pubkey: &Pubkey,
        commitment_config: CommitmentConfig,
    ) -> Result<Option<Account>, ClientError> {
        let _permit = self.pool.acquire().await;

        tokio::task::spawn_blocking({
            let account_pubkey = *account_pubkey;
            let rpc_client = self.rpc_client.clone();
            move || -> Result<Option<Account>, ClientError> {
                rpc_client
                    .get_account_with_commitment(&account_pubkey, commitment_config)
                    .map(|response| response.value)
            }
        })
        .await
        .map_err(|err| {
            ClientError::from(ClientErrorKind::Custom(format!(
                "Failed to get btcana account: {}",
                err
            )))
        })?
    }

    async fn get_program_accounts_with_config(
        &self,
        program_pubkey: &Pubkey,
        config: RpcProgramAccountsConfig,
    ) -> Result<Vec<(Pubkey, Account)>, ClientError> {
        let _permit = self.pool.acquire().await;

        tokio::task::spawn_blocking({
            let program_pubkey = *program_pubkey;
            let rpc_client = self.rpc_client.clone();
            move || -> Result<Vec<(Pubkey, Account)>, ClientError> {
                rpc_client.get_program_accounts_with_config(&program_pubkey, config)
            }
        })
        .await
        .map_err(|err| {
            ClientError::from(ClientErrorKind::Custom(format!(
                "Failed to get btcana program accounts: {}",
                err
            )))
        })?
    }

    async fn get_transaction_async(
        &self,
        tx_id: &Txid,
    ) -> Result<Option<bitcoin::blockdata::transaction::Transaction>, anyhow::Error> {
        let _permit = self.pool.acquire().await;

        tokio::task::spawn_blocking({
            let tx_id = *tx_id;
            let rpc_client = self.rpc_client.clone();
            move || -> Result<Option<bitcoin::blockdata::transaction::Transaction>, anyhow::Error> { rpc_client.get_tx(&tx_id) }
        })
        .await
        .map_err(|err| anyhow::bail!("Failed to get btc transaction: {}", err))?
    }

    async fn get_health(&self) -> Result<(), ClientError> {
        let _permit = self.pool.acquire().await;

        tokio::task::spawn_blocking({
            let rpc_client = self.rpc_client.clone();
            move || -> Result<(), ClientError> { rpc_client.get_health() }
        })
        .await
        .map_err(|err| {
            ClientError::from(ClientErrorKind::Custom(format!(
                "Failed to get btcana health: {}",
                err
            )))
        })?
    }

    async fn verify_btc_ton_transaction(
        &self,
        data: BtcTonTransactionData,
    ) -> Result<VerificationStatus> {
        let result = self.get_transaction(&data.tx_id).await?;

        if !result.output.contains(&TxOut {
            value: data.amount,
            script_pubkey: data.btc_receiver,
        }) {
            return Ok(VerificationStatus::NotExists);
        }

        // TODO: Verify unique btc receiver!

        Ok(VerificationStatus::NotExists)
    }

    async fn verify_btc_ton_account(&self, data: BtcTonAccountData) -> Result<VerificationStatus> {
        // TODO: is here need any checks?
        Ok(VerificationStatus::Exists)
    }

    async fn send_and_confirm_message(
        &self,
        message: Message,
        keystore: &Arc<KeyStore>,
    ) -> Result<Signature, ClientError> {
        let _permit = self.pool.acquire().await;

        tokio::task::spawn_blocking({
            let rpc_client = self.rpc_client.clone();
            let keystore = keystore.clone();
            move || -> Result<Signature, ClientError> {
                let transaction = keystore
                    .btc
                    .sign(message, rpc_client.get_latest_blockhash()?)
                    .map_err(|err| {
                        ClientError::from(ClientErrorKind::Custom(format!(
                            "Failed to sign btc message: {}",
                            err
                        )))
                    })?;

                rpc_client.send_and_confirm_transaction(&transaction)
            }
        })
        .await
        .map_err(|err| {
            ClientError::from(ClientErrorKind::Custom(format!(
                "Failed to send btcana request: {}",
                err
            )))
        })?
    }
}

pub struct TonBtcPendingEvent {
    event_data: TonBtcEventData,
    status_tx: Option<VerificationStatusTx>,
    created_at: u64,
    delay: u32,
    time: u64,
}

pub struct TonBtcEventData {
    pub amount: u64,
    pub receiver: Txid,
}

pub struct BtcTonAccountData {
    pub receiver: MsgAddressInt,
    pub amount: u64,
}

pub struct BtcTonTransactionData {
    pub tx_id: Txid,
    pub block_height: u32,
    pub btc_receiver: Script,
    pub amount: u64,
}

impl TonBtcPendingEvent {
    fn check(&self, _event_data: Vec<u8>) -> VerificationStatus {
        // if self.event_data == event_data {
        VerificationStatus::Exists
        // } else {
        //     VerificationStatus::NotExists
        // }
    }
}

type VerificationStatusTx = oneshot::Sender<VerificationStatus>;

#[derive(Debug, Copy, Clone)]
pub struct BtcSubscriberMetrics {
    pub unrecognized_proposals_count: usize,
}

#[derive(thiserror::Error, Debug)]
enum BtcSubscriberError {
    #[error("Failed to decode btc transaction `{0}`")]
    DecodeTransactionError(String),
    #[error("Relay is not in the round `{0}`")]
    InvalidRound(u32),
    #[error("Relay is not in the round `{0}`")]
    InvalidVotePosition(usize),
    #[error("Relay round `{0}` doesn't exist")]
    InvalidRoundAccount(String),
    #[error("Proposal `{0}` doesn't exist")]
    InvalidProposalAccount(String),
}
