use std::collections::hash_map;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use bitcoin::hash_types::Txid;
use bitcoin::{Script, TxOut};
use esplora_client::Builder;
use rustc_hash::FxHashMap;
use tokio::sync::{oneshot, Semaphore};
use ton_block::MsgAddressInt;

use crate::config::*;
use crate::engine::bridge::*;
use crate::utils::*;

pub struct BtcSubscriber {
    config: BtcConfig,
    rpc_client: Arc<esplora_client::AsyncClient>,
    pool: Arc<Semaphore>,
    pending_events: tokio::sync::Mutex<FxHashMap<MsgAddressInt, TonBtcPendingEvent>>,
    sent_transactions: tokio::sync::Mutex<FxHashMap<Txid, Vec<MsgAddressInt>>>,
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
            sent_transactions: Default::default(),
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
        let rx = {
            let mut pending_events = self.pending_events.lock().await;

            let (tx, rx) = oneshot::channel();

            let created_at = chrono::Utc::now().timestamp() as u64;

            const INIT_INTERVAL_DELAY_SEC: u32 = 300;

            pending_events.insert(
                event_addr,
                TonBtcPendingEvent {
                    event_data: TonBtcEventData { amount, receiver },
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

    pub async fn is_already_voted(&self, _round_number: u32) -> Result<bool> {
        // TODO: CHECK is already voted
        Ok(false)
    }

    async fn update(&self) -> Result<()> {
        tracing::info!(
            pending_events = self.pending_events_count.load(Ordering::Acquire),
            "updating BTC subscriber",
        );

        let sent_transactions = self.get_confirmed_sent_transactions().await?;

        let mut pending_events = self.pending_events.lock().await;

        for (_sent_transaction, events) in sent_transactions {
            for event in events {
                if let hash_map::Entry::Occupied(mut entry) = pending_events.entry(event) {
                    if let Some(tx) = entry.get_mut().status_tx.take() {
                        tx.send(VerificationStatus::Exists).ok();
                    }
                    entry.remove();
                }

                self.pending_events_count
                    .store(pending_events.len(), Ordering::Release);
            }
        }

        drop(pending_events);

        Ok(())
    }

    async fn get_confirmed_sent_transactions(
        &self,
    ) -> Result<FxHashMap<bitcoin::blockdata::transaction::Transaction, Vec<MsgAddressInt>>> {
        let mut result: FxHashMap<
            bitcoin::blockdata::transaction::Transaction,
            Vec<MsgAddressInt>,
        > = Default::default();
        let sent_transaction = self.sent_transactions.lock().await.clone();
        for (tx_id, events) in sent_transaction {
            let transaction_status = {
                retry(
                    || async { self.get_transaction_status_async(&tx_id).await },
                    generate_default_timeout_config(Duration::from_secs(
                        self.config.maximum_failed_responses_time_sec,
                    )),
                    NetworkType::BTC,
                    "get btc transaction",
                )
                .await?
            };

            if let Some(transaction_status) = transaction_status {
                if transaction_status.confirmed {
                    let transaction = {
                        retry(
                            || async { self.get_transaction_async(&tx_id).await },
                            generate_default_timeout_config(Duration::from_secs(
                                self.config.maximum_failed_responses_time_sec,
                            )),
                            NetworkType::BTC,
                            "get btc transaction",
                        )
                        .await?
                    }
                    .ok_or(anyhow::format_err!(
                        "Transaction {} not found",
                        tx_id.to_string()
                    ))?;
                    result.insert(transaction, events);
                }
            }
        }
        Ok(result)
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
            .await
            .map_err(|e| {
                anyhow::format_err!("Can not get transaction {} from rpc - {}", tx_id.to_string(), e.to_string())
            })?
        }
        .ok_or(anyhow::format_err!(
            "Transaction {} not found",
            tx_id.to_string()
        ))?;

        Ok(transaction)
    }

    async fn get_transaction_async(
        &self,
        tx_id: &Txid,
    ) -> Result<Option<bitcoin::blockdata::transaction::Transaction>, anyhow::Error> {
        let _permit = self.pool.acquire().await;

        self.rpc_client
            .get_tx(&tx_id)
            .await
            .map_err(|err| anyhow::format_err!("Failed to get btc transaction: {}", err))
    }

    async fn get_transaction_status_async(
        &self,
        tx_id: &Txid,
    ) -> Result<Option<esplora_client::TxStatus>, anyhow::Error> {
        let _permit = self.pool.acquire().await;

        self.rpc_client
            .get_tx_status(&tx_id)
            .await
            .map_err(|err| anyhow::format_err!("Failed to get btc transaction status: {}", err))
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

    async fn verify_btc_ton_account(&self, _data: BtcTonAccountData) -> Result<VerificationStatus> {
        // TODO: is here need any checks?
        Ok(VerificationStatus::Exists)
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
    pub receiver: Script,
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

type VerificationStatusTx = oneshot::Sender<VerificationStatus>;

#[derive(Debug, Copy, Clone)]
pub struct BtcSubscriberMetrics {
    pub unrecognized_proposals_count: usize,
}

#[derive(thiserror::Error, Debug)]
enum BtcSubscriberError {
}
