use std::collections::hash_map;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::Result;
use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use nekoton_utils::TrustMe;
use rustc_hash::FxHashMap;
use tokio::sync::{Mutex, Semaphore, mpsc};
use ton_block::{GetRepresentationHash, MsgAddressInt};
use ton_types::UInt256;
use tvm_rpc_client::RpcClient;

use crate::utils::*;

const POLLING_INTERVAL_SECS: u64 = 3;
const POOL_SIZE: usize = 15;

pub struct TvmSubscriber {
    current_utime: AtomicU32,
    signature_id: SignatureId,
    account_subscriptions: Mutex<FxHashMap<UInt256, AccountSubscription>>,
    polling_interval: Duration,
    pool: Semaphore,
    messages_queue: Arc<PendingMessagesQueue>,
    rpc_client: RpcClient,
}

impl TvmSubscriber {
    pub fn new(messages_queue: Arc<PendingMessagesQueue>, rpc_client: RpcClient) -> Arc<Self> {
        Arc::new(Self {
            current_utime: Default::default(),
            signature_id: SignatureId::default(),
            account_subscriptions: Mutex::new(FxHashMap::with_capacity_and_hasher(
                128,
                Default::default(),
            )),
            polling_interval: Duration::from_secs(POLLING_INTERVAL_SECS),
            pool: Semaphore::new(POOL_SIZE),
            messages_queue,
            rpc_client,
        })
    }

    pub fn metrics(&self) -> TonSubscriberMetrics {
        TonSubscriberMetrics {
            current_utime: self.current_utime(),
            signature_id: self.signature_id(),
            pending_message_count: self.messages_queue.len(),
        }
    }

    pub fn initialize(
        self: &Arc<Self>,
        blockchain_config: &ton_executor::BlockchainConfig,
    ) -> Result<()> {
        tracing::info!("starting TVM subscriber");
        self.update_signature_id(blockchain_config)?;
        self.update_current_utime();
        tracing::info!("TVM subscriber started");

        Ok(())
    }

    pub fn start(self: &Arc<Self>) -> Result<()> {
        let this = self.clone();
        tokio::spawn(this.start_polling());

        Ok(())
    }

    async fn start_polling(self: Arc<Self>) {
        tracing::info!("Starting polling");
        loop {
            self.update_current_utime();

            if let Err(err) = self.clone().poll_transactions().await {
                tracing::error!("Error while polling transactions: {}", err);
            }

            tokio::time::sleep(self.polling_interval).await;
        }
    }

    pub fn current_utime(&self) -> u32 {
        self.current_utime.load(Ordering::Acquire)
    }

    pub fn signature_id(&self) -> Option<i32> {
        self.signature_id.load()
    }

    pub async fn add_transactions_subscription<I, T>(&self, accounts: I, subscription: &Arc<T>)
    where
        I: IntoIterator<Item = UInt256>,
        T: TransactionsSubscription + 'static,
    {
        let mut state_subscriptions = self.account_subscriptions.lock().await;

        let weak = Arc::downgrade(subscription) as Weak<dyn TransactionsSubscription>;

        for account in accounts {
            match state_subscriptions.entry(account) {
                hash_map::Entry::Vacant(entry) => {
                    let latest_lt = self.get_latest_lt_for_account(&account).await;
                    entry.insert(AccountSubscription {
                        latest_lt: latest_lt.into(),
                        transaction_subscriptions: vec![weak.clone()],
                    });
                }
                hash_map::Entry::Occupied(mut entry) => {
                    entry.get_mut().transaction_subscriptions.push(weak.clone());
                }
            };
        }
    }

    async fn get_latest_lt_for_account(&self, account: &UInt256) -> u64 {
        self.get_contract_state_with_retry(account, 5)
            .await
            .ok()
            .flatten()
            .map(|c| c.last_transaction_id.lt())
            .unwrap_or_default()
    }

    pub async fn get_contract_state_with_retry(
        &self,
        account: &UInt256,
        max_retries: u32,
    ) -> Result<Option<ExistingContract>> {
        let mut attempt = 0;

        loop {
            match self.get_contract_state(account).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    attempt += 1;
                    if attempt >= max_retries {
                        return Err(e);
                    } else {
                        let delay_between_retries = Duration::from_secs(5);
                        tracing::warn!(
                            attempt = attempt,
                            delay_secs = delay_between_retries.as_secs(),
                            error = e.to_string(),
                            "Retrying to get contract state..."
                        );
                        tokio::time::sleep(delay_between_retries).await;
                    }
                }
            }
        }
    }

    pub async fn get_contract_state(&self, account: &UInt256) -> Result<Option<ExistingContract>> {
        let account_id = ton_types::AccountId::from(account);
        let address = &MsgAddressInt::with_standart(None, 0, account_id)?;
        let state = self.rpc_client.get_contract_state(address, None).await;
        state.map(|state_opt| {
            state_opt.map(|state| ExistingContract {
                account: state.account,
                last_transaction_id: state.last_transaction_id,
            })
        })
    }

    pub async fn wait_contract_state(&self, account: &UInt256) -> Result<ExistingContract> {
        loop {
            let Some(contract_state) = self.get_contract_state(account).await? else {
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            };

            match &contract_state.account.storage.state {
                ton_block::AccountState::AccountActive { .. } => {
                    return Ok(contract_state);
                }
                ton_block::AccountState::AccountFrozen { .. } => {
                    return Err(TonSubscriberError::AccountIsFrozen.into());
                }
                ton_block::AccountState::AccountUninit => {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    continue;
                }
            }
        }
    }

    pub async fn get_transactions(
        &self,
        account: &UInt256,
        oldest_transaction_lt: u64,
        last_transaction_lt: Option<u64>,
    ) -> Result<Vec<ton_block::Transaction>> {
        const TRANSACTION_LIMIT: u8 = 100;

        tracing::debug!(account = %DisplayAddr(account), "getting transactions");

        let account_id = ton_types::AccountId::from(account);
        let address = &MsgAddressInt::with_standart(None, 0, account_id)?;
        let mut transactions = self
            .rpc_client
            .get_transactions(TRANSACTION_LIMIT, address, last_transaction_lt)
            .await?;
        if transactions.len() < TRANSACTION_LIMIT as usize {
            transactions.retain(|transaction| transaction.lt >= oldest_transaction_lt);
            return Ok(transactions);
        }

        loop {
            let oldest_retrieved_transaction = transactions.last().trust_me();
            if oldest_transaction_lt >= oldest_retrieved_transaction.lt {
                break;
            }

            let next_batch = self
                .rpc_client
                .get_transactions(
                    TRANSACTION_LIMIT,
                    address,
                    Some(oldest_retrieved_transaction.prev_trans_lt),
                )
                .await?;
            let next_batch_len = next_batch.len();
            if next_batch_len == 0 {
                break;
            }
            transactions.extend(next_batch);
            if next_batch_len < TRANSACTION_LIMIT as usize {
                break;
            }
        }

        transactions.retain(|transaction| transaction.lt >= oldest_transaction_lt);
        Ok(transactions)
    }

    async fn poll_transactions(self: &Arc<Self>) -> Result<()> {
        let mut subscriptions = self.account_subscriptions.lock().await;
        subscriptions.retain(|_, subscription| {
            let subscription_status = subscription.update_status();
            subscription_status != TransactionSubscriptionsStatus::Stopped
        });

        let tasks = FuturesUnordered::new();
        for (account, subscription) in subscriptions.iter() {
            let this = self.clone();
            let latest_lt = subscription.get_latest_lt();
            let account = *account;

            tasks.push(tokio::spawn(async move {
                let _permit = this.pool.acquire().await;
                let account_state = match this.get_contract_state(&account).await {
                    Ok(Some(account_state)) => account_state,
                    Ok(None) => {
                        tracing::warn!(address = %DisplayAddr(account), "Account does not exist");
                        return None;
                    }
                    Err(e) => {
                        tracing::error!(address = %DisplayAddr(account), "No contract state for account: {e:?}");
                        return None;
                    }
                };

                let transactions =
                    match this.get_transactions(&account, latest_lt + 1, None).await {
                    Ok(transactions) => transactions,
                    Err(e) => {
                        tracing::error!(address = %DisplayAddr(account), "Failed to poll transactions: {e:?}");
                        return None;
                    }
                };

                Some((account, (account_state, transactions)))
            }));
        }

        let transactions_map: FxHashMap<_, _> = tasks
            .filter_map(|task| async { task.ok().flatten() })
            .collect()
            .await;

        for (account, subscription) in subscriptions.iter() {
            let Some((account_state, transactions)) = transactions_map.get(account) else {
                tracing::error!(address = %DisplayAddr(account), "Failed to retrieve transactions");
                continue;
            };
            if let Err(e) = subscription.process_transactions(
                &self.messages_queue,
                account_state,
                &mut transactions.clone(),
                account,
            ) {
                tracing::error!(address = %DisplayAddr(account), "Failed to process transactions: {e:?}");
            }
        }

        for (account, (_, transactions)) in transactions_map.iter() {
            let minimal_transaction_utime = transactions
                .iter()
                .map(|t| t.now)
                .min()
                .unwrap_or(self.current_utime());
            self.messages_queue
                .update(account, minimal_transaction_utime);
        }

        Ok(())
    }

    fn update_current_utime(&self) {
        let current_utime = chrono::Utc::now().timestamp() as u32;

        self.current_utime.store(current_utime, Ordering::Release);
    }

    #[cfg(not(feature = "ton"))]
    fn update_signature_id(&self, config: &ton_executor::BlockchainConfig) -> Result<()> {
        self.signature_id
            .store(config.capabilites(), config.global_id());

        Ok(())
    }

    #[cfg(feature = "ton")]
    fn update_signature_id(&self, config: &ton_executor::BlockchainConfig) -> Result<()> {
        self.signature_id.store(0x0, config.global_id());

        Ok(())
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct TonSubscriberMetrics {
    pub current_utime: u32,
    pub signature_id: Option<i32>,
    pub pending_message_count: usize,
}

struct AccountSubscription {
    latest_lt: AtomicU64,
    transaction_subscriptions: Vec<Weak<dyn TransactionsSubscription>>,
}

impl AccountSubscription {
    fn get_latest_lt(&self) -> u64 {
        self.latest_lt.load(Ordering::Acquire)
    }

    fn update_status(&mut self) -> TransactionSubscriptionsStatus {
        self.transaction_subscriptions
            .retain(|item| item.strong_count() > 0);

        if !self.transaction_subscriptions.is_empty() {
            TransactionSubscriptionsStatus::Alive
        } else {
            TransactionSubscriptionsStatus::Stopped
        }
    }

    fn process_transactions(
        &self,
        messages_queue: &PendingMessagesQueue,
        account_state: &ExistingContract,
        transactions: &mut [ton_block::Transaction],
        account: &UInt256,
    ) -> Result<()> {
        if self.transaction_subscriptions.is_empty() {
            return Ok(());
        }

        sort_maybe_desc_to_asc(transactions, |t| t.lt);
        for transaction in transactions.iter() {
            let hash = transaction.hash()?;

            // Skip non-ordinary
            let transaction_info = match transaction.description.read_struct() {
                Ok(ton_block::TransactionDescr::Ordinary(info)) => info,
                _ => continue,
            };

            let in_msg = match transaction
                .in_msg
                .as_ref()
                .map(|message| (message, message.read_struct()))
            {
                Some((message_cell, Ok(message))) => {
                    if message.is_inbound_external() {
                        messages_queue.deliver_message(
                            *account,
                            message_cell.hash(),
                            transaction_info.aborted,
                        );
                    }
                    message
                }
                _ => continue,
            };

            // Skip aborted transactions
            if transaction_info.aborted {
                continue;
            }

            let ctx = TxContext {
                account_state,
                account,
                transaction_hash: &hash,
                transaction_info: &transaction_info,
                transaction,
                in_msg: &in_msg,
            };

            // Handle transaction
            for subscription in self.iter_transaction_subscriptions() {
                if let Err(e) = subscription.handle_transaction(ctx) {
                    tracing::error!(
                        tx = hash.to_hex_string(),
                        account = %DisplayAddr(account),
                        "Failed to handle transaction: {e:?}",
                    );
                }
            }

            self.latest_lt.store(transaction.lt, Ordering::Release);
        }

        Ok(())
    }

    fn iter_transaction_subscriptions(
        &'_ self,
    ) -> impl Iterator<Item = Arc<dyn TransactionsSubscription>> + '_ {
        self.transaction_subscriptions
            .iter()
            .filter_map(Weak::upgrade)
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum TransactionSubscriptionsStatus {
    Alive,
    Stopped,
}

pub trait TransactionsSubscription: Send + Sync {
    fn handle_transaction(&self, ctx: TxContext<'_>) -> Result<()>;
}

/// Generic listener for transactions
pub struct AccountObserver<T>(AccountEventsTx<T>);

impl<T> AccountObserver<T> {
    pub fn new(tx: &AccountEventsTx<T>) -> Arc<Self> {
        Arc::new(Self(tx.clone()))
    }
}

impl<T> TransactionsSubscription for AccountObserver<T>
where
    T: ReadFromTransaction + std::fmt::Debug + Send + Sync,
{
    fn handle_transaction(&self, ctx: TxContext<'_>) -> Result<()> {
        let event = T::read_from_transaction(&ctx);

        // Send event to event manager if it exists
        if let Some(event) = event {
            tracing::info!(
                ?event,
                account = %DisplayAddr(ctx.account),
                "got transaction on account",
            );
            if self.0.send((*ctx.account, event)).is_err() {
                tracing::error!(
                    account = %DisplayAddr(ctx.account),
                    "failed to send event: channel is dropped",
                );
            }
        }

        // Done
        Ok(())
    }
}

pub fn start_listening_events<S, E, R>(
    service: &Arc<S>,
    name: &'static str,
    mut events_rx: mpsc::UnboundedReceiver<E>,
    handler: fn(Arc<S>, E) -> R,
) where
    S: Send + Sync + 'static,
    E: Send + 'static,
    R: futures_util::Future<Output = Result<()>> + Send + 'static,
{
    let service = Arc::downgrade(service);

    tokio::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            let service = match service.upgrade() {
                Some(service) => service,
                None => break,
            };

            if let Err(e) = handler(service, event).await {
                tracing::error!(contract = name, "failed to handle event: {e:?}");
            }
        }

        tracing::warn!(contract = name, "stopped listening for events");

        events_rx.close();
        while events_rx.recv().await.is_some() {}
    });
}

#[derive(Default)]
struct SignatureId(AtomicU64);

impl SignatureId {
    const WITH_SIGNATURE_ID: u64 = 1 << 32;

    fn load(&self) -> Option<i32> {
        let id = self.0.load(Ordering::Acquire);
        if id & Self::WITH_SIGNATURE_ID != 0 {
            Some(id as i32)
        } else {
            None
        }
    }

    fn store(&self, capabilities: u64, global_id: i32) {
        const CAP_WITH_SIGNATURE_ID: u64 = 0x4000000;
        let id = if capabilities & CAP_WITH_SIGNATURE_ID != 0 {
            Self::WITH_SIGNATURE_ID | (global_id as u32 as u64)
        } else {
            0
        };
        self.0.store(id, Ordering::Release);
    }
}

pub type AccountEventsTx<T> = mpsc::UnboundedSender<(UInt256, T)>;

#[derive(thiserror::Error, Debug)]
enum TonSubscriberError {
    #[error("Account is frozen")]
    AccountIsFrozen,
}

#[cfg(test)]
mod tests {
    use crate::engine::tvm_subscriber::{SignatureId, TvmSubscriber};
    use crate::utils::{PendingMessagesQueue, only_account_hash};
    use nekoton_utils::TrustMe;
    use std::str::FromStr;
    use ton_block::MsgAddressInt;
    use tvm_rpc_client::RpcClient;
    use url::Url;

    #[test]
    fn test_signature_id() {
        let signature_id = SignatureId::default();

        signature_id.store(0x4000000, 1337);
        let id = signature_id.load();
        assert_eq!(id, Some(1337));
    }

    #[test]
    fn test_signature_id_without_capabilities() {
        let signature_id = SignatureId::default();

        signature_id.store(0x0, 1337);
        let id = signature_id.load();
        assert_eq!(id, None);
    }

    #[tokio::test]
    #[ignore]
    async fn transactions_playground() {
        let rpc_url = Url::parse("https://jrpc-ton.broxus.com").unwrap();
        let rpc_client = RpcClient::new(vec![rpc_url], Default::default())
            .await
            .trust_me();
        let tvm_subscriber = TvmSubscriber::new(PendingMessagesQueue::new(0), rpc_client);

        let account = MsgAddressInt::from_str(
            "0:a519f99bb5d6d51ef958ed24d337ad75a1c770885dcd42d51d6663f9fcdacfb2",
        )
        .trust_me();
        let account = only_account_hash(account);
        let latest_lt = 54948624000006;
        let txs = tvm_subscriber
            .get_transactions(&account, latest_lt + 1, None)
            .await
            .trust_me();

        assert!(txs.len() > 100);
    }
}
