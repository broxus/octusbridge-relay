use std::collections::{hash_map, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use rustc_hash::FxHashMap;
use solana_account_decoder::UiAccountEncoding;
use tokio::sync::{oneshot, Semaphore};

use solana_client::client_error::{ClientError, ClientErrorKind};
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::{
    RpcAccountInfoConfig, RpcProgramAccountsConfig, RpcTransactionConfig,
};
use solana_client::rpc_filter::{Memcmp, MemcmpEncodedBytes, RpcFilterType};
use solana_sdk::account::{Account, ReadableAccount};
use solana_sdk::bs58;
use solana_sdk::clock::{Slot, UnixTimestamp};
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::message::Message;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use solana_transaction_status::{EncodedConfirmedTransaction, UiTransactionEncoding};

use crate::config::*;
use crate::engine::bridge::*;
use crate::engine::keystore::*;
use crate::utils::*;

pub struct SolSubscriber {
    config: SolConfig,
    rpc_client: Arc<RpcClient>,
    pool: Arc<Semaphore>,
    programs_to_subscribe: parking_lot::RwLock<Vec<Pubkey>>,
    pending_events: tokio::sync::Mutex<FxHashMap<Pubkey, PendingEvent>>,
    pending_events_count: AtomicUsize,
}

impl SolSubscriber {
    pub async fn new(config: SolConfig) -> Result<Arc<Self>> {
        let rpc_client = Arc::new(RpcClient::new_with_timeout_and_commitment(
            config.endpoint.clone(),
            Duration::from_secs(config.connection_timeout_sec),
            config.commitment,
        ));

        let pool = Arc::new(Semaphore::new(config.pool_size));

        let subscriber = Arc::new(Self {
            config,
            rpc_client,
            pool,
            programs_to_subscribe: Default::default(),
            pending_events: Default::default(),
            pending_events_count: Default::default(),
        });

        Ok(subscriber)
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
                    log::error!(
                        "Error occurred during solana node subscriber update: {:?}",
                        e
                    );
                }

                tokio::time::sleep(Duration::from_secs(subscriber.config.poll_interval_sec)).await;
            }
        });
    }

    pub fn subscribe(&self, program_pubkey: Pubkey) {
        let mut programs = self.programs_to_subscribe.write();
        if !programs.contains(&program_pubkey) {
            log::info!("Subscribe to `{}` solana program", program_pubkey);
            programs.push(program_pubkey);
        }
    }

    pub async fn verify_ton_sol_event(
        &self,
        account_pubkey: Pubkey,
        event_data: Vec<u8>,
    ) -> Result<(VerificationStatus, u32)> {
        let rx = {
            let mut pending_events = self.pending_events.lock().await;

            let (tx, rx) = oneshot::channel();

            pending_events.insert(
                account_pubkey,
                PendingEvent {
                    event_data,
                    status_tx: Some(tx),
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

    pub async fn verify_sol_ton_event(
        &self,
        transaction_data: SolTonTransactionData,
        account_data: SolTonAccountData,
    ) -> Result<VerificationStatus> {
        if self.verify_sol_ton_transaction(transaction_data).await? == VerificationStatus::NotExists
        {
            return Ok(VerificationStatus::NotExists);
        }

        self.verify_sol_ton_account(account_data).await
    }

    pub async fn send_message(&self, message: Message, keystore: &Arc<KeyStore>) -> Result<()> {
        let _ = {
            let _permit = self.pool.acquire().await;

            retry(
                || async {
                    let message = message.clone();
                    self.send_and_confirm_message(message, keystore).await
                },
                generate_sol_rpc_backoff_config(Duration::from_secs(
                    self.config.maximum_failed_responses_time_sec,
                )),
                "send solana transaction",
            )
            .await
            .context("Failed sending solana message")?
        };

        Ok(())
    }

    async fn update(&self) -> Result<()> {
        log::info!(
            "TON->SOL pending events: {}",
            self.pending_events_count.load(Ordering::Acquire)
        );

        let mut accounts_to_check = HashMap::new();

        // Get pending Solana proposals to check
        let programs_to_subscribe = self.programs_to_subscribe.read().clone();
        for program_pubkey in programs_to_subscribe {
            let pending_proposals = self.get_pending_proposals(&program_pubkey).await?;
            let pending_proposals = pending_proposals
                .into_iter()
                .map(|(pubkey, account)| (pubkey, Some(account)))
                .collect::<HashMap<Pubkey, Option<Account>>>();
            accounts_to_check.extend(pending_proposals);
        }

        // Get pending TON events to check
        let time = chrono::Utc::now().timestamp() as u64;

        let mut pending_events = self.pending_events.lock().await;
        for (account, event) in pending_events.iter_mut() {
            if !accounts_to_check.contains_key(account) && time > event.time {
                event.time = time + 3600; // Shift to 1 hour
                accounts_to_check.insert(*account, None);
            }
        }

        log::info!(
            "Accounts to check: {:?}",
            accounts_to_check
                .iter()
                .map(|(pubkey, _)| pubkey)
                .collect::<Vec<&Pubkey>>()
        );

        // Check accounts
        for (account_pubkey, account_data) in accounts_to_check {
            let result = match account_data {
                Some(account_data) => Ok(Some(account_data)),
                None => self.get_account(&account_pubkey).await,
            };

            match result {
                Ok(Some(account)) => {
                    if let hash_map::Entry::Occupied(mut entry) =
                        pending_events.entry(account_pubkey)
                    {
                        let account_data =
                            solana_bridge::bridge_state::Proposal::unpack_from_slice(
                                account.data(),
                            )?;

                        let status = entry.get().check(account_data.event);
                        let round_number = account_data.round_number;

                        if let Some(tx) = entry.get_mut().status_tx.take() {
                            tx.send((status, round_number)).ok();
                        }

                        entry.remove();
                    }
                }
                Ok(None) => {
                    log::info!(
                        "Solana proposal account `{}` doesn't exist yet",
                        account_pubkey
                    );
                }
                Err(err) => {
                    log::error!(
                        "Failed to check solana proposal `{}`: {:?}",
                        account_pubkey,
                        err
                    );
                }
            }
        }

        self.pending_events_count
            .store(pending_events.len(), Ordering::Release);

        drop(pending_events);

        Ok(())
    }

    async fn get_account(&self, account_pubkey: &Pubkey) -> Result<Option<Account>> {
        let account = {
            let _permit = self.pool.acquire().await;

            retry(
                || self.get_account_with_commitment(account_pubkey, self.config.commitment),
                generate_default_timeout_config(Duration::from_secs(
                    self.config.maximum_failed_responses_time_sec,
                )),
                "get account",
            )
            .await
            .context("Failed getting solana account")?
        };

        Ok(account)
    }

    async fn get_pending_proposals(
        &self,
        program_pubkey: &Pubkey,
    ) -> Result<Vec<(Pubkey, Account)>> {
        let accounts = {
            let _permit = self.pool.acquire().await;

            retry(
                || async {
                    let mem: Vec<u8> = vec![
                        true as u8,                                               // is_initialized
                        solana_bridge::bridge_state::AccountKind::Proposal as u8, // account_kind
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
                            ..Default::default()
                        },
                        ..Default::default()
                    };

                    self.get_program_accounts_with_config(program_pubkey, config)
                        .await
                },
                generate_default_timeout_config(Duration::from_secs(
                    self.config.maximum_failed_responses_time_sec,
                )),
                "get account",
            )
            .await
            .context("Failed getting solana account")?
        };

        Ok(accounts)
    }

    async fn get_transaction(&self, signature: &Signature) -> Result<EncodedConfirmedTransaction> {
        let transaction = {
            let _permit = self.pool.acquire().await;

            retry(
                || async {
                    let config = RpcTransactionConfig {
                        commitment: Some(self.config.commitment),
                        encoding: Some(UiTransactionEncoding::Base64),
                    };

                    self.get_transaction_with_config(signature, config).await
                },
                generate_default_timeout_config(Duration::from_secs(
                    self.config.maximum_failed_responses_time_sec,
                )),
                "get solana transaction",
            )
            .await
            .context("Failed getting solana transaction")?
        };

        Ok(transaction)
    }

    async fn get_account_with_commitment(
        &self,
        account_pubkey: &Pubkey,
        commitment_config: CommitmentConfig,
    ) -> Result<Option<Account>, ClientError> {
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
                "Failed to send solana request: {}",
                err
            )))
        })?
    }

    async fn get_program_accounts_with_config(
        &self,
        program_pubkey: &Pubkey,
        config: RpcProgramAccountsConfig,
    ) -> Result<Vec<(Pubkey, Account)>, ClientError> {
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
                "Failed to send solana request: {}",
                err
            )))
        })?
    }

    async fn get_transaction_with_config(
        &self,
        signature: &Signature,
        config: RpcTransactionConfig,
    ) -> Result<EncodedConfirmedTransaction, ClientError> {
        tokio::task::spawn_blocking({
            let signature = *signature;
            let rpc_client = self.rpc_client.clone();
            move || -> Result<EncodedConfirmedTransaction, ClientError> {
                rpc_client.get_transaction_with_config(&signature, config)
            }
        })
        .await
        .map_err(|err| {
            ClientError::from(ClientErrorKind::Custom(format!(
                "Failed to send solana request: {}",
                err
            )))
        })?
    }

    async fn verify_sol_ton_transaction(
        &self,
        data: SolTonTransactionData,
    ) -> Result<VerificationStatus> {
        let result = self.get_transaction(&data.signature).await?;

        if result.slot != data.slot || result.block_time != Some(data.block_time) {
            return Ok(VerificationStatus::NotExists);
        }

        let transaction = result.transaction.transaction.decode().ok_or_else(|| {
            SolSubscriberError::DecodeTransactionError(data.signature.to_string())
        })?;

        for ix in transaction.message.instructions {
            if transaction.message.account_keys[ix.program_id_index as usize] == data.program_id {
                let deposit_seed = u128::from_le_bytes(ix.data[1..17].try_into()?);
                if deposit_seed == data.seed {
                    return Ok(VerificationStatus::Exists);
                }
            }
        }

        Ok(VerificationStatus::NotExists)
    }

    async fn verify_sol_ton_account(&self, data: SolTonAccountData) -> Result<VerificationStatus> {
        let account_pubkey = solana_bridge::token_proxy::get_associated_deposit_address(
            &data.program_id,
            data.seed,
            &data.settings,
        );

        let result = self.get_account(&account_pubkey).await?;

        let account = match result {
            Some(account) => account,
            None => {
                log::error!("Solana account {} not exist", account_pubkey);
                return Ok(VerificationStatus::NotExists);
            }
        };

        let account_data = solana_bridge::token_proxy::Deposit::unpack_from_slice(account.data())?;
        if data.event_data != account_data.event {
            return Ok(VerificationStatus::NotExists);
        }

        Ok(VerificationStatus::Exists)
    }

    async fn send_and_confirm_message(
        &self,
        message: Message,
        keystore: &Arc<KeyStore>,
    ) -> Result<Signature, ClientError> {
        tokio::task::spawn_blocking({
            let transaction = keystore
                .sol
                .sign(message, self.rpc_client.get_latest_blockhash()?)
                .map_err(|err| {
                    ClientError::from(ClientErrorKind::Custom(format!(
                        "Failed to sign sol message: {}",
                        err
                    )))
                })?;
            let rpc_client = self.rpc_client.clone();
            move || -> Result<Signature, ClientError> {
                rpc_client.send_and_confirm_transaction(&transaction)
            }
        })
        .await
        .map_err(|err| {
            ClientError::from(ClientErrorKind::Custom(format!(
                "Failed to send solana request: {}",
                err
            )))
        })?
    }
}

struct PendingEvent {
    event_data: Vec<u8>,
    status_tx: Option<VerificationStatusTx>,
    time: u64,
}

impl PendingEvent {
    fn check(&self, event_data: Vec<u8>) -> VerificationStatus {
        if self.event_data == event_data {
            VerificationStatus::Exists
        } else {
            VerificationStatus::NotExists
        }
    }
}

type VerificationStatusTx = oneshot::Sender<(VerificationStatus, u32)>;

pub struct SolTonAccountData {
    pub program_id: Pubkey,
    pub settings: Pubkey,
    pub seed: u128,
    pub event_data: Vec<u8>,
}

pub struct SolTonTransactionData {
    pub program_id: Pubkey,
    pub signature: Signature,
    pub slot: Slot,
    pub block_time: UnixTimestamp,
    pub seed: u128,
}

#[derive(thiserror::Error, Debug)]
enum SolSubscriberError {
    #[error("Failed to decode solana transaction: `{0}`")]
    DecodeTransactionError(String),
}
