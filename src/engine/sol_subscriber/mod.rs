use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tiny_adnl::utils::*;
use tokio::sync::{oneshot, Notify};

use solana_client::rpc_client::RpcClient;
use solana_sdk::account::{Account, ReadableAccount};
use solana_sdk::hash::Hash;
use solana_sdk::program_pack::Pack;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use solana_sdk::transaction::Transaction;
use token_proxy::WithdrawalPattern;
use ton_types::UInt256;

use crate::config::*;
use crate::engine::bridge::*;
use crate::utils::*;

pub struct SolSubscriber {
    config: SolConfig,
    rpc_client: Arc<RpcClient>,
    ton_pending_confirmations: tokio::sync::Mutex<FxHashMap<EventId, TonSolPendingConfirmation>>,
    ton_pending_confirmation_count: AtomicUsize,
    ton_new_events_notify: Notify,
}

impl SolSubscriber {
    pub async fn new(config: SolConfig) -> Result<Arc<Self>> {
        let rpc_client = Arc::new(RpcClient::new_with_commitment(
            &config.url,
            config.commitment_config,
        ));

        let subscriber = Arc::new(Self {
            config,
            rpc_client,
            ton_pending_confirmations: Default::default(),
            ton_pending_confirmation_count: Default::default(),
            ton_new_events_notify: Notify::new(),
        });

        Ok(subscriber)
    }

    pub fn metrics(&self) -> SolSubscriberMetrics {
        SolSubscriberMetrics {
            ton_pending_confirmation_count: self
                .ton_pending_confirmation_count
                .load(Ordering::Acquire),
        }
    }

    pub fn start(self: &Arc<Self>) {
        let subscriber = Arc::downgrade(self);

        tokio::spawn(async move {
            loop {
                let subscriber = match subscriber.upgrade() {
                    Some(subscriber) => subscriber,
                    None => return,
                };

                tokio::select! {
                    _ = subscriber.ton_new_events_notify.notified() => {},
                    _ = tokio::time::sleep(Duration::from_secs(subscriber.config.poll_interval_sec)) => {},
                };

                if let Err(e) = subscriber.ton_update().await {
                    log::error!("Error occurred during Solana event update: {:?}", e);
                }
            }
        });
    }

    async fn ton_update(&self) -> Result<()> {
        log::info!(
            "TON->SOL pending confirmations: {}",
            self.ton_pending_confirmation_count.load(Ordering::Acquire)
        );

        let pending_confirmations = self.ton_pending_confirmations.lock().await;
        let event_ids = pending_confirmations
            .iter()
            .map(|(event_id, _)| *event_id)
            .collect::<Vec<EventId>>();
        drop(pending_confirmations);

        for event_id in event_ids {
            let account_pubkey =
                token_proxy::get_associated_withdrawal_address(&event_id.0, event_id.1);

            // Prepare tryhard config
            let api_request_strategy = generate_fixed_timeout_config(
                Duration::from_secs(self.config.get_timeout_sec),
                Duration::from_secs(self.config.maximum_failed_responses_time_sec),
            );

            let account = match retry(
                || self.get_account(&account_pubkey),
                api_request_strategy,
                "get solana account",
            )
            .await
            {
                Ok(account) => account,
                Err(e) if is_account_not_found(&e, &account_pubkey) => {
                    log::info!(
                        "Withdrawal Solana Account 0x{} not created yet",
                        account_pubkey
                    );
                    continue;
                }
                Err(e) => {
                    log::error!(
                        "Failed to get Withdrawal Solana Account 0x{}: {}",
                        account_pubkey,
                        e
                    );
                    continue;
                }
            };

            let account_data = token_proxy::WithdrawalPattern::unpack(account.data())?;

            let mut pending_confirmations = self.ton_pending_confirmations.lock().await;
            if let Some(confirmation) = pending_confirmations.get_mut(&event_id) {
                let status = confirmation.check(account_data);

                log::info!("Confirmation status: {:?}", status);

                if let Some(tx) = confirmation.status_tx.take() {
                    tx.send(status).ok();
                }
            }
            drop(pending_confirmations);
        }

        Ok(())
    }

    async fn get_account(&self, account_pubkey: &Pubkey) -> Result<Account> {
        self.rpc_client
            .get_account(account_pubkey)
            .map_err(anyhow::Error::new)
    }

    async fn get_latest_blockhash(&self) -> Result<Hash> {
        self.rpc_client
            .get_latest_blockhash()
            .map_err(anyhow::Error::new)
    }

    async fn send_and_confirm_transaction(&self, transaction: &Transaction) -> Result<Signature> {
        self.rpc_client
            .send_and_confirm_transaction(transaction)
            .map_err(anyhow::Error::new)
    }

    pub async fn verify_ton_event(
        &self,
        configuration: UInt256,
        event_transaction_lt: u64,
        event_data: Vec<u8>,
    ) -> Result<VerificationStatus> {
        let rx = {
            let (tx, rx) = oneshot::channel();

            let event_id = (configuration, event_transaction_lt);

            let mut pending_confirmations = self.ton_pending_confirmations.lock().await;
            pending_confirmations.insert(
                event_id,
                TonSolPendingConfirmation {
                    event_data,
                    status_tx: Some(tx),
                },
            );

            self.ton_pending_confirmation_count
                .store(pending_confirmations.len(), Ordering::Release);

            self.ton_new_events_notify.notify_waiters();

            rx
        };

        let status = rx.await?;
        Ok(status)
    }

    pub async fn get_recent_blockhash(&self) -> Result<Hash> {
        let api_request_strategy = generate_fixed_timeout_config(
            Duration::from_secs(self.config.get_timeout_sec),
            Duration::from_secs(self.config.maximum_failed_responses_time_sec),
        );

        let latest_blockhash = match retry(
            || self.get_latest_blockhash(),
            api_request_strategy,
            "get latest blockhash",
        )
        .await
        {
            Ok(latest_blockhash) => latest_blockhash,
            Err(e) => {
                return Err(e).with_context(|| "Failed to get latest Solana blockhash".to_string())
            }
        };

        Ok(latest_blockhash)
    }

    pub async fn send_transaction(&self, transaction: Transaction) -> Result<()> {
        // Prepare tryhard config
        let api_request_strategy = generate_fixed_timeout_config(
            Duration::from_secs(self.config.get_timeout_sec),
            Duration::from_secs(self.config.maximum_failed_responses_time_sec),
        );

        match retry(
            || self.send_and_confirm_transaction(&transaction),
            api_request_strategy,
            "send solana transaction",
        )
        .await
        {
            Ok(_) => {}
            Err(e) => {
                return Err(e).with_context(|| "Failed to send Solana transaction".to_string())
            }
        };

        Ok(())
    }
}

#[derive(Debug, Copy, Clone)]
pub struct SolSubscriberMetrics {
    pub ton_pending_confirmation_count: usize,
}

struct TonSolPendingConfirmation {
    event_data: Vec<u8>,
    status_tx: Option<VerificationStatusTx>,
}

impl TonSolPendingConfirmation {
    fn check(&self, account: WithdrawalPattern) -> VerificationStatus {
        if self.event_data != account.event {
            return VerificationStatus::NotExists;
        }

        VerificationStatus::Exists
    }
}

type VerificationStatusTx = oneshot::Sender<VerificationStatus>;

fn is_account_not_found(error: &anyhow::Error, pubkey: &Pubkey) -> bool {
    error
        .to_string()
        .contains(format!("AccountNotFound: pubkey={}", pubkey).as_str())
}

pub type EventId = (UInt256, u64);
