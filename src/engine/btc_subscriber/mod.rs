use std::collections::{hash_map, BTreeMap};
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use bitcoin::blockdata::transaction;
use bitcoin::hash_types::Txid;
use bitcoin::hashes::hex::ToHex;
use bitcoin::hashes::Hash;
use bitcoin::util::psbt;
use bitcoin::{BlockHash, OutPoint, PackedLockTime, Script, Witness};
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
    in_utxos: tokio::sync::Mutex<FxHashMap<Utxo, u64>>,
    pending_events: tokio::sync::Mutex<FxHashMap<UInt256, PendingEvent>>,
    pending_withdrawals: tokio::sync::Mutex<FxHashMap<UInt256, Vec<(Script, u64)>>>,
    pending_event_count: AtomicUsize,
    new_events_notify: Notify,
}

impl BtcSubscriber {
    pub async fn new(config: BtcConfig) -> Result<Arc<Self>> {
        let builder = Builder::new(&config.esplora_url);
        let rpc_client = Arc::new(builder.build_async()?);

        let pool = Arc::new(Semaphore::new(config.pool_size));

        let address_tracker = Arc::new(tracker::AddressTracker::new(&config.bkp_public_keys)?);

        let subscriber = Arc::new(Self {
            config,
            pool,
            rpc_client,
            address_tracker,
            in_utxos: Default::default(),
            pending_events: Default::default(),
            pending_withdrawals: Default::default(),
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
        let tx_id = Txid::from_hash(Hash::from_slice(vote_data.transaction.as_array())?);

        let rx = {
            let mut pending_events = self.pending_events.lock().await;

            let (tx, rx) = oneshot::channel();

            let block_hash = BlockHash::from_slice(vote_data.event_block.as_array())?;

            let target_block = vote_data.block_number + blocks_to_confirm as u32;

            // TODO: get TSS pubkey from everscale-network
            let master_xpub = bitcoin::util::bip32::ExtendedPubKey::from_str("").unwrap();

            let btc_receiver = self
                .address_tracker
                .generate_script_pubkey(master_xpub, deposit_account_id)?;

            let event_data = BtcTonEventData {
                tx_id,
                block_hash,
                btc_receiver,
                amount: vote_data.amount,
                block_height: vote_data.block_number,
                output_index: vote_data.output_index,
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
            let mut in_utxos = self.in_utxos.lock().await;
            in_utxos.insert(
                Utxo {
                    tx_id,
                    vout: vote_data.output_index,
                },
                vote_data.amount,
            );
        }

        Ok(status)
    }

    pub async fn create_btc_transaction(
        &self,
        account: UInt256,
    ) -> Result<transaction::Transaction> {
        let withdrawals = self
            .pending_withdrawals
            .lock()
            .await
            .get(&account)
            .ok_or(BtcSubscriberError::WithdrawalsNotFound(
                account.to_hex_string(),
            ))?
            .clone();

        // Count the total output value
        let total_out = withdrawals
            .iter()
            .fold(0u64, |total, (_, amount)| total + amount);

        // TODO: get fee
        let fee = 0;

        // All outputs to spend
        let in_utxos = self.in_utxos.lock().await;

        // Sort existing outputs by balance
        let mut in_utxos_vec: Vec<(&Utxo, &u64)> = in_utxos.iter().collect();
        in_utxos_vec.sort_by(|a, b| b.1.cmp(a.1));

        // Outputs to spend for current withdrawal
        let mut in_outputs = vec![];
        {
            let mut total = 0;
            for (output, amount) in in_utxos_vec {
                in_outputs.push((output.clone(), amount));
                total += amount;

                if total >= total_out {
                    break;
                }
            }
        }

        // Check all given inputs
        let total_in = in_outputs
            .iter()
            .fold(0u64, |total, (_, &amount)| total + amount);

        if total_out + fee > total_in {
            return Err(BtcSubscriberError::InsufficientBalance.into());
        }

        // Get inputs
        let input: Vec<_> = in_outputs
            .into_iter()
            .map(|(utxo, _)| transaction::TxIn {
                previous_output: OutPoint {
                    txid: utxo.tx_id,
                    vout: utxo.vout,
                },
                script_sig: Script::new(),
                witness: Witness::default(),
                sequence: transaction::Sequence::default(),
            })
            .collect();

        // Get outputs
        let mut output: Vec<_> = withdrawals
            .into_iter()
            .map(|(script_pubkey, value)| transaction::TxOut {
                value,
                script_pubkey,
            })
            .collect();

        // Add change.
        let change_amount = total_in - total_out - fee;

        // TODO: get TSS pubkey from everscale-network
        let master_xpub = bitcoin::util::bip32::ExtendedPubKey::from_str("").unwrap();
        let change_addr = self
            .address_tracker
            .generate_script_pubkey(master_xpub, 0)?;

        output.push(transaction::TxOut {
            value: change_amount,
            script_pubkey: change_addr,
        });

        let psbt = psbt::Psbt {
            unsigned_tx: transaction::Transaction {
                version: 2,
                lock_time: PackedLockTime::ZERO,
                input,
                output,
            },
            unknown: BTreeMap::new(),
            proprietary: BTreeMap::new(),
            xpub: BTreeMap::new(),
            version: 0,
            inputs: vec![],
            outputs: vec![],
        };

        // TODO: sign and finalize

        let tx = psbt.extract_tx();

        Ok(tx)
    }

    pub async fn commit_btc_ton(&self, vote_data: BtcTonEventVoteData) -> Result<()> {
        let tx_id = Txid::from_hash(Hash::from_slice(vote_data.transaction.as_array())?);

        let mut in_utxos = self.in_utxos.lock().await;
        in_utxos.insert(
            Utxo {
                tx_id,
                vout: vote_data.output_index,
            },
            vote_data.amount,
        );

        Ok(())
    }

    pub async fn commit_ton_btc(&self, tx: &[u8]) -> Result<()> {
        let tx: transaction::Transaction = bitcoin::consensus::deserialize(tx)?;

        let mut in_utxos = self.in_utxos.lock().await;
        for tx_in in tx.input {
            in_utxos.remove(&Utxo {
                tx_id: tx_in.previous_output.txid,
                vout: tx_in.previous_output.vout,
            });
        }

        Ok(())
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

    pub async fn get_pending_withdrawals(&self) -> Vec<UInt256> {
        self.pending_withdrawals
            .lock()
            .await
            .iter()
            .map(|(&account, _)| account)
            .collect()
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
        } else if tx_data.tx.txid() != event_data.tx_id {
            Err(format!(
                "Tx Id mismatch. From event: {}. Expected: {}",
                event_data.tx_id,
                tx_data.tx.txid(),
            ))
        } else if !tx_data.tx.output.contains(&transaction::TxOut {
            value: event_data.amount,
            script_pubkey: event_data.btc_receiver.clone(),
        }) {
            Err(format!(
                "Btc receiver mismatch. Output where address - {} and amount - {} not found.",
                event_data.btc_receiver.to_hex(),
                event_data.amount,
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
    pub output_index: u32,
    pub block_height: u32,
    pub btc_receiver: Script,
    pub block_hash: BlockHash,
}

#[derive(Debug, Clone)]
pub struct BtcTransactionData {
    pub tx: transaction::Transaction,
    pub tx_status: esplora_client::TxStatus,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
struct Utxo {
    /// The referenced transaction's txid.
    tx_id: Txid,
    /// The index of the referenced output in its transaction's vout.
    vout: u32,
}

type VerificationStatusTx = oneshot::Sender<VerificationStatus>;

#[derive(thiserror::Error, Debug)]
enum BtcSubscriberError {
    #[error("BTC withdrawals `{0}` not found")]
    WithdrawalsNotFound(String),
    #[error("Insufficient BTC balance to make withdrawal")]
    InsufficientBalance,
}
