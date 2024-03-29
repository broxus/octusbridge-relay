use std::collections::{hash_map, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use rustc_hash::FxHashMap;
use tokio::sync::{oneshot, Notify, Semaphore};

use solana_bridge::bridge_state::Proposal;
use solana_client::client_error::{ClientError, ClientErrorKind};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::RpcTransactionConfig;
use solana_sdk::account::{Account, ReadableAccount};
use solana_sdk::bs58;
use solana_sdk::clock::{Slot, UnixTimestamp};
use solana_sdk::instruction::CompiledInstruction;
use solana_sdk::message::{Message, VersionedMessage};
use solana_sdk::program_pack::Pack;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use solana_transaction_status::option_serializer::OptionSerializer;
use solana_transaction_status::{
    EncodedConfirmedTransactionWithStatusMeta, UiInstruction, UiTransactionEncoding,
};

use crate::config::*;
use crate::engine::bridge::*;
use crate::engine::keystore::*;
use crate::utils::*;

static ROUND_ROBIN_COUNTER: AtomicUsize = AtomicUsize::new(0);

pub struct SolSubscriber {
    config: SolConfig,
    rpc_clients: Vec<SolClient>,
    programs_to_subscribe: parking_lot::RwLock<Vec<Pubkey>>,
    pending_events: tokio::sync::Mutex<FxHashMap<Pubkey, PendingEvent>>,
    pending_events_count: AtomicUsize,
    new_events_notify: Notify,
}

impl SolSubscriber {
    pub async fn new(config: SolConfig) -> Result<Arc<Self>> {
        let rpc_clients = config
            .endpoints
            .iter()
            .map(|endpoint| SolClient {
                rpc_client: RpcClient::new_with_timeout(
                    endpoint.clone(),
                    Duration::from_secs(config.connection_timeout_sec),
                ),
                pool: Semaphore::new(config.pool_size),
            })
            .collect::<Vec<_>>();

        for client in rpc_clients.iter() {
            let block_height =
                get_block_height(client, config.maximum_failed_responses_time_sec).await?;
            tracing::info!(
                block_height,
                url = &client.rpc_client.url(),
                "SOL subscriber"
            );
        }

        let subscriber = Arc::new(Self {
            config,
            rpc_clients,
            programs_to_subscribe: Default::default(),
            pending_events: Default::default(),
            pending_events_count: Default::default(),
            new_events_notify: Notify::new(),
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
                    tracing::error!("error occurred during SOL subscriber update: {e:?}");
                }
            }
        });
    }

    pub fn subscribe(&self, program_pubkey: Pubkey) {
        let mut programs = self.programs_to_subscribe.write();
        if !programs.contains(&program_pubkey) {
            tracing::info!(%program_pubkey, "subscribe to solana program");
            programs.push(program_pubkey);
        }
    }

    pub fn metrics(&self) -> SolSubscriberMetrics {
        SolSubscriberMetrics {
            pending_events_count: self.pending_events_count.load(Ordering::Acquire),
        }
    }

    pub async fn verify_ton_sol_event(
        &self,
        account_pubkey: Pubkey,
        event_data: Vec<u8>,
        created_at: u32,
    ) -> Result<VerificationStatus> {
        let rx = {
            let mut pending_events = self.pending_events.lock().await;

            let (tx, rx) = oneshot::channel();

            const INIT_INTERVAL_DELAY_SEC: u32 = 300; // 5 min

            pending_events.insert(
                account_pubkey,
                PendingEvent {
                    event_data,
                    status_tx: Some(tx),
                    created_at: created_at as u64,
                    delay: INIT_INTERVAL_DELAY_SEC,
                    time: Default::default(),
                },
            );

            self.pending_events_count
                .store(pending_events.len(), Ordering::Release);

            self.new_events_notify.notify_waiters();

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
        let client = self.get_rpc_client()?;
        match verify_sol_ton_transaction(client, transaction_data, &self.config).await? {
            VerificationStatus::Exists => {
                verify_sol_ton_account(client, account_data, &self.config).await
            }
            status @ VerificationStatus::NotExists { .. } => Ok(status),
        }
    }

    pub async fn send_message(
        &self,
        client: &SolClient,
        message: Message,
        keystore: &Arc<KeyStore>,
    ) -> Result<Signature, ClientError> {
        let signature = {
            retry(
                || async {
                    let message = message.clone();
                    send_and_confirm_message(
                        client,
                        message,
                        keystore,
                        self.config.maximum_failed_responses_time_sec,
                    )
                    .await
                },
                generate_sol_rpc_backoff_config(Duration::from_secs(
                    self.config.maximum_failed_responses_time_sec,
                )),
                NetworkType::SOL,
                "send solana transaction",
            )
            .await?
        };

        Ok(signature)
    }

    pub async fn get_signature_status(
        &self,
        client: &SolClient,
        signature: &Signature,
    ) -> Result<(), ClientError> {
        let res = loop {
            let status = {
                retry(
                    || async {
                        let _permit = client.pool.acquire().await;
                        client.rpc_client.get_signature_status(signature).await
                    },
                    generate_default_timeout_config(Duration::from_secs(
                        self.config.maximum_failed_responses_time_sec,
                    )),
                    NetworkType::SOL,
                    "get transaction",
                )
                .await?
            };

            if let Some(res) = status {
                break res;
            }

            // Transaction has not yet been finalized. Retry after timeout.
            tokio::time::sleep(Duration::from_secs(
                self.config.poll_signature_status_interval_sec,
            ))
            .await;
        };

        match res {
            Ok(()) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn is_already_voted(
        &self,
        client: &SolClient,
        round_number: u32,
        proposal_pubkey: &Pubkey,
        voter_pubkey: &Pubkey,
    ) -> Result<bool> {
        let maximum_failed_responses_time_secs = self.config.maximum_failed_responses_time_sec;

        let relay_round_pubkey = solana_bridge::round_loader::get_relay_round_address(round_number);
        let relay_round_account = get_account(
            client,
            &relay_round_pubkey,
            maximum_failed_responses_time_secs,
        )
        .await?;
        let relay_round_account_data = match relay_round_account {
            Some(account) => solana_bridge::round_loader::RelayRound::unpack(account.data())?,
            None => {
                return Err(
                    SolSubscriberError::InvalidRoundAccount(relay_round_pubkey.to_string()).into(),
                )
            }
        };

        let proposal_account =
            get_account(client, proposal_pubkey, maximum_failed_responses_time_secs).await?;
        let proposal_data = match proposal_account {
            Some(account) => Proposal::unpack_from_slice(account.data())?,
            None => {
                // Here only in case if event is expired. Don't vote in Solana
                return Ok(true);
            }
        };

        let index = relay_round_account_data
            .relays
            .iter()
            .position(|pubkey| pubkey == voter_pubkey)
            .ok_or(SolSubscriberError::InvalidRound(round_number))?;

        let vote = proposal_data
            .signers
            .get(index)
            .ok_or(SolSubscriberError::InvalidVotePosition(index))?;

        Ok(*vote != solana_bridge::bridge_types::Vote::None)
    }

    async fn update(&self) -> Result<()> {
        tokio::select! {
            _ = self.new_events_notify.notified() => {},
            _ = tokio::time::sleep(Duration::from_secs(self.config.poll_interval_sec)) => {},
        }

        let rpc_client = self.get_rpc_client()?;
        let maximum_failed_responses_time_secs = self.config.maximum_failed_responses_time_sec;

        tracing::info!(
            pending_events = self.pending_events_count.load(Ordering::Acquire),
            "updating SOL subscriber",
        );

        let mut accounts_to_check = HashSet::new();

        // Get pending TON events to check
        let time = chrono::Utc::now().timestamp() as u64;

        let mut pending_events = self.pending_events.lock().await;
        pending_events.retain(|account, event| {
            const EXPIRED_PERIOD: u64 = 14 * 24 * 60 * 60; // 14 days
            match time > event.created_at + EXPIRED_PERIOD {
                true => {
                    if let Some(tx) = event.status_tx.take() {
                        tx.send(VerificationStatus::NotExists {
                            reason: "TON->SOL event is expired".to_owned(),
                        })
                        .ok();
                    }

                    false
                }
                false => {
                    if time > event.time {
                        tracing::info!(
                            account_pubkey = %account,
                            "adding proposal account from TON->SOL pending events to checklist",
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

                    true
                }
            }
        });

        if !accounts_to_check.is_empty() {
            tracing::info!(?accounts_to_check);
        }

        // Check accounts
        for account_pubkey in accounts_to_check {
            match get_account(
                rpc_client,
                &account_pubkey,
                maximum_failed_responses_time_secs,
            )
            .await
            {
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

                        let round_pubkey = solana_bridge::round_loader::get_relay_round_address(
                            account_data.round_number,
                        );
                        match get_account(
                            rpc_client,
                            &round_pubkey,
                            maximum_failed_responses_time_secs,
                        )
                        .await
                        {
                            Ok(None) => {
                                entry.remove();

                                anyhow::bail!(
                                    "Round {} in solana doesn't exist",
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
                        "Solana proposal account doesn't exist yet",
                    );
                }
                Err(e) => {
                    tracing::error!(
                        %account_pubkey,
                        "failed to check solana proposal: {e:?}",
                    );
                }
            }
        }

        self.pending_events_count
            .store(pending_events.len(), Ordering::Release);

        drop(pending_events);

        Ok(())
    }

    pub fn get_rpc_client(&self) -> Result<&SolClient, ClientError> {
        let index = ROUND_ROBIN_COUNTER.fetch_add(1, Ordering::Release) % self.rpc_clients.len();

        self.rpc_clients
            .get(index)
            .ok_or(ClientError::from(ClientErrorKind::Custom(
                "Failed to get solana RPC client".to_string(),
            )))
    }
}

async fn get_account(
    client: &SolClient,
    account_pubkey: &Pubkey,
    maximum_failed_responses_time_secs: u64,
) -> Result<Option<Account>> {
    let account = {
        retry(
            || async {
                let _permit = client.pool.acquire().await;
                client
                    .rpc_client
                    .get_account_with_commitment(account_pubkey, Default::default())
                    .await
                    .map(|response| response.value)
            },
            generate_default_timeout_config(Duration::from_secs(
                maximum_failed_responses_time_secs,
            )),
            NetworkType::SOL,
            "get account",
        )
        .await?
    };

    Ok(account)
}

async fn get_transaction(
    client: &SolClient,
    signature: &Signature,
    maximum_failed_responses_time_secs: u64,
) -> Result<EncodedConfirmedTransactionWithStatusMeta> {
    let transaction = {
        retry(
            || async {
                let config = RpcTransactionConfig {
                    encoding: Some(UiTransactionEncoding::Base64),
                    ..Default::default()
                };

                let _permit = client.pool.acquire().await;
                client
                    .rpc_client
                    .get_transaction_with_config(signature, config)
                    .await
            },
            generate_default_timeout_config(Duration::from_secs(
                maximum_failed_responses_time_secs,
            )),
            NetworkType::SOL,
            "get solana transaction",
        )
        .await?
    };

    Ok(transaction)
}

async fn get_latest_blockhash(
    client: &SolClient,
    maximum_failed_responses_time_secs: u64,
) -> Result<solana_sdk::hash::Hash, ClientError> {
    let hash = {
        retry(
            || async {
                let _permit = client.pool.acquire().await;
                client.rpc_client.get_latest_blockhash().await
            },
            generate_default_timeout_config(Duration::from_secs(
                maximum_failed_responses_time_secs,
            )),
            NetworkType::SOL,
            "get latest blockhash",
        )
        .await?
    };

    Ok(hash)
}

async fn get_block_height(
    client: &SolClient,
    maximum_failed_responses_time_secs: u64,
) -> Result<u64> {
    let block_height = {
        retry(
            || async {
                let _permit = client.pool.acquire().await;
                client.rpc_client.get_block_height().await
            },
            generate_default_timeout_config(Duration::from_secs(
                maximum_failed_responses_time_secs,
            )),
            NetworkType::SOL,
            "get block height",
        )
        .await?
    };

    Ok(block_height)
}

async fn healthcheck(client: &SolClient, maximum_failed_responses_time_secs: u64) -> Result<()> {
    retry(
        || async {
            let _permit = client.pool.acquire().await;
            client.rpc_client.get_health().await
        },
        generate_default_timeout_config(Duration::from_secs(maximum_failed_responses_time_secs)),
        NetworkType::SOL,
        "healthcheck",
    )
    .await?;

    Ok(())
}

async fn send_and_confirm_message(
    client: &SolClient,
    message: Message,
    keystore: &Arc<KeyStore>,
    maximum_failed_responses_time_secs: u64,
) -> Result<Signature, ClientError> {
    let transaction = keystore
        .sol
        .sign(
            message,
            get_latest_blockhash(client, maximum_failed_responses_time_secs).await?,
        )
        .map_err(|err| {
            ClientError::from(ClientErrorKind::Custom(format!(
                "Failed to sign sol message: {err}"
            )))
        })?;

    let _permit = client.pool.acquire().await;
    client
        .rpc_client
        .send_and_confirm_transaction(&transaction)
        .await
}

async fn verify_sol_ton_account(
    client: &SolClient,
    data: SolTonAccountData,
    config: &SolConfig,
) -> Result<VerificationStatus> {
    let account_pubkey =
        solana_bridge::token_proxy::get_associated_deposit_address(&data.program_id, data.seed);

    let now = std::time::Instant::now();

    let result = loop {
        let maximum_failed_responses_time_secs = config.maximum_failed_responses_time_sec;

        healthcheck(client, maximum_failed_responses_time_secs).await?;

        if let Some(account) =
            get_account(client, &account_pubkey, maximum_failed_responses_time_secs).await?
        {
            break Some(account);
        }

        if now.elapsed().as_secs() > config.poll_deposits_timeout_sec {
            break None;
        }

        tokio::time::sleep(Duration::from_secs(config.poll_deposits_interval_sec)).await;
    };

    let account = match result {
        Some(account) => account,
        None => {
            tracing::error!(%account_pubkey, "Solana account doesn't exist");
            return Ok(VerificationStatus::NotExists {
                reason: "Solana account doesn't exist".to_owned(),
            });
        }
    };

    let account_data = solana_bridge::token_proxy::Deposit::unpack_from_slice(account.data())?;
    if data.event_data != account_data.event {
        return Ok(VerificationStatus::NotExists {
            reason: "Event data mismatch".to_owned(),
        });
    }

    Ok(VerificationStatus::Exists)
}

async fn verify_sol_ton_transaction(
    client: &SolClient,
    data: SolTonTransactionData,
    config: &SolConfig,
) -> Result<VerificationStatus> {
    let maximum_failed_responses_time_secs = config.maximum_failed_responses_time_sec;

    let result =
        get_transaction(client, &data.signature, maximum_failed_responses_time_secs).await?;

    if result.slot != data.slot || result.block_time != Some(data.block_time) {
        return Ok(VerificationStatus::NotExists {
            reason: "Block slot or time mismatch".to_owned(),
        });
    }

    let transaction =
        result.transaction.transaction.decode().ok_or_else(|| {
            SolSubscriberError::DecodeTransactionError(data.signature.to_string())
        })?;

    let (account_keys, mut instructions) = match transaction.message {
        VersionedMessage::Legacy(message) => (message.account_keys, message.instructions),
        VersionedMessage::V0(message) => (message.account_keys, message.instructions),
    };

    // Handle CPI instructions
    let inner_instructions = result
        .transaction
        .meta
        .as_ref()
        .and_then(|t| {
            if let OptionSerializer::Some(instructions) = &t.inner_instructions {
                Some(
                    instructions
                        .iter()
                        .flat_map(|ix| ix.instructions.clone())
                        .collect::<Vec<_>>(),
                )
            } else {
                None
            }
        })
        .unwrap_or_default();

    for ix in inner_instructions {
        if let UiInstruction::Compiled(ix) = ix {
            let c_ix = CompiledInstruction {
                program_id_index: ix.program_id_index,
                accounts: ix.accounts,
                data: bs58::decode(&ix.data).into_vec()?,
            };
            instructions.push(c_ix);
        }
    }

    for ix in instructions {
        if account_keys[ix.program_id_index as usize] == data.program_id {
            let deposit_seed = u128::from_le_bytes(ix.data[1..17].try_into()?);
            if deposit_seed == data.seed {
                return Ok(VerificationStatus::Exists);
            }
        }
    }

    Ok(VerificationStatus::NotExists {
        reason: "Deposit seed not found".to_owned(),
    })
}

pub struct SolClient {
    rpc_client: RpcClient,
    pool: Semaphore,
}

struct PendingEvent {
    event_data: Vec<u8>,
    status_tx: Option<VerificationStatusTx>,
    created_at: u64,
    delay: u32,
    time: u64,
}

impl PendingEvent {
    fn check(&self, event_data: Vec<u8>) -> VerificationStatus {
        if self.event_data == event_data {
            VerificationStatus::Exists
        } else {
            VerificationStatus::NotExists {
                reason: "Event data mismatch".to_owned(),
            }
        }
    }
}

type VerificationStatusTx = oneshot::Sender<VerificationStatus>;

pub struct SolTonAccountData {
    pub program_id: Pubkey,
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

#[derive(Debug, Copy, Clone)]
pub struct SolSubscriberMetrics {
    pub pending_events_count: usize,
}

#[derive(thiserror::Error, Debug)]
enum SolSubscriberError {
    #[error("Failed to decode solana transaction `{0}`")]
    DecodeTransactionError(String),
    #[error("Relay is not in the round `{0}`")]
    InvalidRound(u32),
    #[error("Invalid vote position `{0}`")]
    InvalidVotePosition(usize),
    #[error("Relay round `{0}` doesn't exist")]
    InvalidRoundAccount(String),
}
