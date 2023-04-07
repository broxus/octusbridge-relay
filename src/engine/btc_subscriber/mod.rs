use std::collections::{hash_map, BTreeSet};
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use bitcoin::blockdata::transaction;
use bitcoin::hash_types::Txid;
use bitcoin::hashes::Hash;
use bitcoin::{BlockHash, Script, TxOut};
use esplora_client::Builder;
use futures_util::StreamExt;
use rustc_hash::FxHashMap;
use tokio::sync::{oneshot, Notify, Semaphore};
use ton_types::UInt256;

use crate::config::*;
use crate::engine::bridge::*;
use crate::engine::ton_contracts::BtcTonEventVoteData;
use crate::utils::*;

pub mod tracker;

pub struct BtcSubscriber {
    config: BtcConfig,
    pool: Arc<Semaphore>,
    rpc_client: Arc<esplora_client::AsyncClient>,
    address_tracker: Arc<tracker::AddressTracker>,
    utxo_balances: tokio::sync::Mutex<BTreeSet<BtcBalance>>,
    pending_events: tokio::sync::Mutex<FxHashMap<UInt256, PendingEvent>>,
    pending_withdrawals: tokio::sync::Mutex<FxHashMap<UInt256, Vec<(Script, u64)>>>,
    pending_event_count: AtomicUsize,
    new_events_notify: Notify,
}

impl BtcSubscriber {
    pub async fn new(config: BtcConfig) -> Result<Arc<Self>> {
        let builder = Builder::new(&config.esplora_url);
        let rpc_client = Arc::new(builder.build_async()?);

        let address_tracker = Arc::new(tracker::AddressTracker::new(&config.bkp_public_keys)?);

        let pool = Arc::new(Semaphore::new(config.pool_size));

        let subscriber = Arc::new(Self {
            config,
            pool,
            rpc_client,
            address_tracker,
            pending_withdrawals: Default::default(),
            utxo_balances: Default::default(),
            pending_events: Default::default(),
            pending_event_count: Default::default(),
            new_events_notify: Notify::new(),
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

    pub async fn verify_btc_ton_event(
        &self,
        account: UInt256,
        blocks_to_confirm: u16,
        deposit_account_id: u32,
        vote_data: BtcTonEventVoteData,
    ) -> Result<VerificationStatus> {
        // TODO: get TSS pubkey from everscale-network
        let master_xpub = bitcoin::util::bip32::ExtendedPubKey::from_str("").unwrap();

        let btc_receiver = self
            .address_tracker
            .generate_script_pubkey(master_xpub, deposit_account_id)?;

        let rx = {
            let mut pending_events = self.pending_events.lock().await;

            let (tx, rx) = oneshot::channel();

            let block_hash = BlockHash::from_slice(vote_data.event_block.as_array())?;
            let tx_id = Txid::from_hash(Hash::from_slice(vote_data.transaction.as_array())?);

            let target_block = vote_data.block_number + blocks_to_confirm as u32;

            let event_data = BtcTonEventData {
                tx_id,
                block_hash,
                amount: vote_data.amount,
                btc_receiver: btc_receiver.clone(),
                block_height: vote_data.block_number,
            };

            pending_events.insert(
                account,
                PendingEvent {
                    event_data,
                    target_block,
                    status_tx: Some(tx),
                },
            );

            self.pending_event_count
                .store(pending_events.len(), Ordering::Release);

            self.new_events_notify.notify_waiters();

            rx
        };

        let status = rx.await?;

        if let VerificationStatus::Exists = status {
            let mut utxo_balances = self.utxo_balances.lock().await;
            utxo_balances.insert(BtcBalance {
                id: btc_receiver,
                balance: vote_data.amount,
            });
        }

        Ok(status)
    }

    pub async fn create_btc_transaction(
        &self,
        account: UInt256,
    ) -> Result<transaction::Transaction> {
        todo!()
    }

    pub async fn add_pending_withdrawal(
        &self,
        account: UInt256,
        id: Script,
        value: u64,
    ) -> Result<()> {
        let mut withdrawal = self.pending_withdrawals.lock().await;
        match withdrawal.entry(account) {
            hash_map::Entry::Vacant(entry) => {
                entry.insert(vec![(id, value)]);
            }
            hash_map::Entry::Occupied(mut entry) => {
                entry.get_mut().push((id, value));
            }
        }

        Ok(())
    }

    pub async fn remove_pending_withdrawals(&self, account: UInt256) -> Result<()> {
        let mut withdrawal = self.pending_withdrawals.lock().await;
        withdrawal.remove(&account);

        Ok(())
    }

    pub async fn commit(
        &self,
        deposit_account_id: u32,
        vote_data: BtcTonEventVoteData,
    ) -> Result<()> {
        // TODO: get TSS pubkey from everscale-network
        let master_xpub = bitcoin::util::bip32::ExtendedPubKey::from_str("").unwrap();

        let btc_receiver = self
            .address_tracker
            .generate_script_pubkey(master_xpub, deposit_account_id)?;

        let mut utxo_balances = self.utxo_balances.lock().await;
        utxo_balances.insert(BtcBalance {
            id: btc_receiver,
            balance: vote_data.amount,
        });

        Ok(())
    }

    pub async fn remove(&self, tx: transaction::Transaction) -> Result<()> {
        let mut utxo_balances = self.utxo_balances.lock().await;
        for out in tx.output {
            utxo_balances.remove(&BtcBalance {
                id: out.script_pubkey,
                balance: out.value,
            });
        }

        Ok(())
    }

    async fn update(&self) -> Result<()> {
        if self.pending_events.lock().await.is_empty() {
            // Wait until new events appeared or idle poll interval passed.
            // NOTE: Idle polling is needed there to prevent large intervals from occurring
            tokio::select! {
                _ = self.new_events_notify.notified() => {},
                _ = tokio::time::sleep(Duration::from_secs(self.config.poll_interval_sec)) => {},
            }
        }

        tracing::info!(
            pending_events = self.pending_event_count.load(Ordering::Acquire),
            "updating BTC->TON subscriber",
        );

        let events_to_check = futures_util::stream::FuturesUnordered::new();

        let pending_events = self.pending_events.lock().await;
        for (&account, event) in pending_events.iter() {
            // Let us to drop mutex before executing futures
            let tx_id = event.event_data.tx_id;

            events_to_check.push(async move {
                let tx = match self.get_transaction(&tx_id).await {
                    Ok(tx) => tx,
                    Err(err) => {
                        return (account, Err(err));
                    }
                };

                let tx_status = match self.get_transaction_status(&tx_id).await {
                    Ok(tx_status) => tx_status,
                    Err(err) => {
                        return (account, Err(err));
                    }
                };

                let result = 'result: {
                    if let Some(tx) = tx {
                        if let Some(tx_status) = tx_status {
                            break 'result Ok(Some(BtcTransactionData { tx, tx_status }));
                        }
                    }

                    break 'result Ok(None);
                };

                (account, result)
            });
        }
        drop(pending_events);

        let current_block = self.get_height().await?;

        let events_to_check = events_to_check
            .collect::<Vec<(UInt256, Result<Option<BtcTransactionData>>)>>()
            .await;

        let mut pending_events = self.pending_events.lock().await;
        for (account, result) in events_to_check {
            if let hash_map::Entry::Occupied(mut entry) = pending_events.entry(account) {
                if current_block >= entry.get().target_block {
                    let status = match result {
                        Ok(Some(data)) => entry.get_mut().check(data),
                        Ok(None) => VerificationStatus::NotExists {
                            reason: "Not found".to_owned(),
                        },
                        Err(e) => {
                            tracing::error!(
                                event = entry.key().to_string(),
                                tx = entry.get().event_data.tx_id.to_string(),
                                "failed to check BTC event: {e:?}",
                            );

                            continue;
                        }
                    };

                    if let Some(tx) = entry.get_mut().status_tx.take() {
                        tx.send(status).ok();
                    }

                    entry.remove();
                }
            }
        }
        drop(pending_events);

        Ok(())
    }

    async fn get_height(&self) -> Result<u32> {
        let height = {
            retry(
                || async { self.get_height_async().await },
                generate_default_timeout_config(Duration::from_secs(
                    self.config.maximum_failed_responses_time_sec,
                )),
                NetworkType::BTC,
                "get btc height",
            )
            .await
            .map_err(|e| anyhow::format_err!("Can not get height from rpc - {}", e.to_string()))?
        };

        Ok(height)
    }

    async fn get_transaction(&self, tx_id: &Txid) -> Result<Option<transaction::Transaction>> {
        let transaction = {
            retry(
                || async { self.get_transaction_async(tx_id).await },
                generate_default_timeout_config(Duration::from_secs(
                    self.config.maximum_failed_responses_time_sec,
                )),
                NetworkType::BTC,
                "get btc transaction",
            )
            .await
            .map_err(|e| {
                anyhow::format_err!(
                    "Can not get transaction {} from rpc - {}",
                    tx_id.to_string(),
                    e.to_string()
                )
            })?
        };

        Ok(transaction)
    }

    async fn get_transaction_status(
        &self,
        tx_id: &Txid,
    ) -> Result<Option<esplora_client::TxStatus>> {
        let status = {
            retry(
                || async { self.get_transaction_status_async(tx_id).await },
                generate_default_timeout_config(Duration::from_secs(
                    self.config.maximum_failed_responses_time_sec,
                )),
                NetworkType::BTC,
                "get btc transaction status",
            )
            .await
            .map_err(|e| {
                anyhow::format_err!(
                    "Can not get transaction status {} from rpc - {}",
                    tx_id.to_string(),
                    e.to_string()
                )
            })?
        };

        Ok(status)
    }

    async fn get_height_async(&self) -> Result<u32, anyhow::Error> {
        let _permit = self.pool.acquire().await;

        self.rpc_client
            .get_height()
            .await
            .map_err(|err| anyhow::format_err!("Failed to get btc height: {}", err))
    }

    async fn get_transaction_async(
        &self,
        tx_id: &Txid,
    ) -> Result<Option<transaction::Transaction>, anyhow::Error> {
        let _permit = self.pool.acquire().await;

        self.rpc_client
            .get_tx(tx_id)
            .await
            .map_err(|err| anyhow::format_err!("Failed to get btc transaction: {}", err))
    }

    async fn get_transaction_status_async(
        &self,
        tx_id: &Txid,
    ) -> Result<Option<esplora_client::TxStatus>, anyhow::Error> {
        let _permit = self.pool.acquire().await;

        self.rpc_client
            .get_tx_status(tx_id)
            .await
            .map_err(|err| anyhow::format_err!("Failed to get btc transaction status: {}", err))
    }
}

pub struct PendingEvent {
    event_data: BtcTonEventData,
    status_tx: Option<VerificationStatusTx>,
    target_block: u32,
}

impl PendingEvent {
    pub fn check(&mut self, tx_data: BtcTransactionData) -> VerificationStatus {
        let event_data = &self.event_data;

        let result = if tx_data
            .tx_status
            .block_hash
            .unwrap_or(BlockHash::all_zeros())
            != event_data.block_hash
        {
            Err(format!(
                "Block hash mismatch. From event: {}. Expected: {}",
                event_data.block_hash,
                tx_data
                    .tx_status
                    .block_hash
                    .unwrap_or(BlockHash::all_zeros())
            ))
        } else if tx_data.tx_status.block_height.unwrap_or_default() != event_data.block_height {
            Err(format!(
                "Block height mismatch. From event: {}. Expected: {}",
                event_data.block_height,
                tx_data.tx_status.block_height.unwrap_or_default()
            ))
        } else if !tx_data.tx.output.contains(&TxOut {
            value: event_data.amount,
            script_pubkey: event_data.btc_receiver.clone(),
        }) {
            Err(format!(
                "UTXO not found: amount - {}, receiver - {}",
                event_data.amount, event_data.btc_receiver
            ))
        } else {
            Ok(())
        };

        match result {
            Ok(()) => VerificationStatus::Exists,
            Err(reason) => VerificationStatus::NotExists { reason },
        }
    }
}

#[derive(Debug, Clone)]
pub struct BtcTonEventData {
    pub tx_id: Txid,
    pub amount: u64,
    pub block_height: u32,
    pub block_hash: BlockHash,
    pub btc_receiver: Script,
}

#[derive(Debug, Clone)]
pub struct BtcTransactionData {
    pub tx: transaction::Transaction,
    pub tx_status: esplora_client::TxStatus,
}

#[derive(Debug, Clone, Eq)]
struct BtcBalance {
    id: Script,
    balance: u64,
}

impl Ord for BtcBalance {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other.balance.cmp(&self.balance).reverse()
    }
}

impl PartialOrd for BtcBalance {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for BtcBalance {
    fn eq(&self, other: &Self) -> bool {
        self.balance == other.balance
    }
}

type VerificationStatusTx = oneshot::Sender<VerificationStatus>;

/*#[derive(thiserror::Error, Debug)]
enum BtcSubscriberError {
    #[error("BTC transaction `{0}` not found")]
    TransactionNotFound(String),
}*/

// Iteration
/*
let mut iter = self
            .db
            .utxo_balance_storage()
            .balances_iterator()
            .peekable();
        while let Some((id, balance)) = iter.next() {}
*/
