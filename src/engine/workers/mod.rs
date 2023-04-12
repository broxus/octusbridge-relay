use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use crate::engine::keystore::UnsignedMessage;
use crate::engine::ton_contracts::{ton_btc_event_contract, TonBtcEventContract};
use crate::engine::EngineContext;

pub struct Workers {
    /// Shared engine context
    context: Arc<EngineContext>,
}

impl Workers {
    pub async fn new(ctx: Arc<EngineContext>, poll_interval_sec: u64) -> Arc<Self> {
        let workers = Arc::new(Workers { context: ctx });

        // Start BTC worker
        {
            let weak = Arc::downgrade(&workers);

            tokio::spawn(async move {
                loop {
                    let w = match weak.upgrade() {
                        Some(w) => w,
                        None => return,
                    };

                    if let Err(e) = w.btc_worker_update().await {
                        tracing::error!("error occurred during BTC worker update: {e:?}");
                    }

                    tokio::time::sleep(Duration::from_secs(poll_interval_sec)).await;
                }
            });
        }

        workers
    }

    async fn btc_worker_update(&self) -> Result<()> {
        let btc_subscriber = match &self.context.btc_subscriber {
            Some(btc_subscriber) => btc_subscriber,
            _ => return Ok(()),
        };

        let withdrawals = btc_subscriber.get_pending_withdrawals().await;
        for account in withdrawals {
            let keystore = &self.context.keystore;
            let ton_subscriber = &self.context.ton_subscriber;

            // Wait contract state
            let contract = ton_subscriber.wait_contract_state(account).await?;

            let now = chrono::Utc::now().timestamp() as u32;
            let end = TonBtcEventContract(&contract)
                .event_init_data()?
                .vote_data
                .end;

            if now > end {
                let unsigned_message =
                    UnsignedMessage::new(ton_btc_event_contract::finalize(), account);

                let signature_id = ton_subscriber.signature_id();
                let message = keystore.ton.sign(&unsigned_message, signature_id)?;

                if let Err(e) = self
                    .context
                    .send_ton_message(&message.account, &message.message, message.expire_at)
                    .await
                {
                    tracing::error!("failed to send 'finalize' to '{account}': {e:?}");
                }
            }
        }

        Ok(())
    }
}
