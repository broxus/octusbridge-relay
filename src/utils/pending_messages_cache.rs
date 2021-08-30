use std::collections::hash_map;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::Result;
use parking_lot::Mutex;
use tiny_adnl::utils::*;
use tokio::sync::oneshot;
use ton_types::UInt256;

use super::shard_utils::*;

pub struct PendingMessagesCache {
    min_expire_at: AtomicU32,
    entries: Mutex<FxHashMap<PendingMessageId, PendingMessage>>,
}

impl PendingMessagesCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            min_expire_at: AtomicU32::new(u32::MAX),
            entries: Mutex::new(FxHashMap::with_capacity_and_hasher(
                capacity,
                Default::default(),
            )),
        }
    }

    pub fn add_message(
        &self,
        account: UInt256,
        message_hash: UInt256,
        expire_at: u32,
    ) -> Result<MessageStatusRx> {
        let mut entries = self.entries.lock();

        match entries.entry(PendingMessageId {
            account,
            message_hash,
        }) {
            hash_map::Entry::Vacant(entry) => {
                let (tx, rx) = oneshot::channel();
                entry.insert(PendingMessage {
                    tx: Some(tx),
                    expire_at,
                });

                self.min_expire_at.fetch_min(expire_at, Ordering::AcqRel);

                Ok(rx)
            }
            hash_map::Entry::Occupied(_) => Err(PendingMessagesCacheError::AlreadyExists.into()),
        }
    }

    pub fn deliver_message(&self, account: UInt256, message_hash: UInt256) {
        let mut entries = self.entries.lock();
        let mut message = match entries.remove(&PendingMessageId {
            account,
            message_hash,
        }) {
            Some(message) => message,
            None => return,
        };

        if let Some(tx) = message.tx.take() {
            tx.send(MessageStatus::Delivered).ok();
        }

        let current_min_expire_at = self.min_expire_at.load(Ordering::Acquire);
        if current_min_expire_at != message.expire_at {
            return;
        }

        let mut min_expire_at: u32 = u32::MAX;
        entries.iter().for_each(|(_, item)| {
            if item.expire_at < min_expire_at {
                min_expire_at = item.expire_at;
            }
        });

        self.min_expire_at.store(min_expire_at, Ordering::Release);
    }

    pub fn update(&self, shard: &ton_block::ShardIdent, current_utime: u32) {
        let current_min_expire_at = self.min_expire_at.load(Ordering::Acquire);
        if current_utime <= current_min_expire_at {
            return;
        }

        let mut min_expire_at: u32 = u32::MAX;

        let mut entries = self.entries.lock();
        entries.retain(|id, item| {
            if current_utime <= item.expire_at || !contains_account(shard, &id.account) {
                if item.expire_at < min_expire_at {
                    min_expire_at = item.expire_at;
                }
                return true;
            }

            if let Some(tx) = item.tx.take() {
                tx.send(MessageStatus::Expired).ok();
            }
            false
        });

        self.min_expire_at.store(min_expire_at, Ordering::Release);
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum MessageStatus {
    Delivered,
    Expired,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
struct PendingMessageId {
    account: UInt256,
    message_hash: UInt256,
}

struct PendingMessage {
    tx: Option<MessageStatusTx>,
    expire_at: u32,
}

type MessageStatusTx = oneshot::Sender<MessageStatus>;
type MessageStatusRx = oneshot::Receiver<MessageStatus>;

#[derive(thiserror::Error, Debug)]
enum PendingMessagesCacheError {
    #[error("Already exists")]
    AlreadyExists,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_hash(id: u8) -> UInt256 {
        let mut hash = [0; 32];
        hash[0] = id;
        UInt256::from(hash)
    }

    #[tokio::test]
    async fn test_cache_consistency() {
        let cache = PendingMessagesCache::new(10);
        assert_eq!(cache.min_expire_at.load(Ordering::Acquire), u32::MAX);

        // 1. Normal message flow

        // Add message
        let rx = cache.add_message(make_hash(0), make_hash(0), 10).unwrap();

        // (Adding same message should fail)
        assert!(cache.add_message(make_hash(0), make_hash(0), 20).is_err());
        // Adding new message must update expiration
        assert_eq!(cache.min_expire_at.load(Ordering::Acquire), 10);

        // Deliver message
        cache.deliver_message(make_hash(0), make_hash(0));
        assert_eq!(cache.min_expire_at.load(Ordering::Acquire), u32::MAX);
        assert_eq!(rx.await.unwrap(), MessageStatus::Delivered);

        // 2. Expired message flow

        // Add message
        let rx = cache.add_message(make_hash(0), make_hash(0), 10).unwrap();

        // Update before expiration time must not do anything
        cache.update(&ton_block::ShardIdent::masterchain(), 5);
        assert_eq!(cache.min_expire_at.load(Ordering::Acquire), 10);

        // Update after expiration time must remove message
        cache.update(&ton_block::ShardIdent::masterchain(), 15);
        assert_eq!(cache.min_expire_at.load(Ordering::Acquire), u32::MAX);
        assert_eq!(rx.await.unwrap(), MessageStatus::Expired);

        // 3. Multiple messages expiration flow

        // Add messages
        let rx2 = cache.add_message(make_hash(1), make_hash(1), 20).unwrap();
        let rx1 = cache.add_message(make_hash(0), make_hash(0), 10).unwrap();

        cache.update(&ton_block::ShardIdent::masterchain(), 5);
        assert_eq!(cache.min_expire_at.load(Ordering::Acquire), 10);

        cache.update(&ton_block::ShardIdent::masterchain(), 10);
        assert_eq!(cache.min_expire_at.load(Ordering::Acquire), 10);

        cache.update(&ton_block::ShardIdent::masterchain(), 15);
        assert_eq!(cache.min_expire_at.load(Ordering::Acquire), 20);

        cache.update(&ton_block::ShardIdent::masterchain(), 25);
        assert_eq!(cache.min_expire_at.load(Ordering::Acquire), u32::MAX);

        assert_eq!(rx1.await.unwrap(), MessageStatus::Expired);
        assert_eq!(rx2.await.unwrap(), MessageStatus::Expired);

        // 4. Multiple message delivery flow

        // Add messages
        let rx2 = cache.add_message(make_hash(1), make_hash(1), 20).unwrap();
        let rx1 = cache.add_message(make_hash(0), make_hash(0), 10).unwrap();

        cache.update(&ton_block::ShardIdent::masterchain(), 5);
        assert_eq!(cache.min_expire_at.load(Ordering::Acquire), 10);

        cache.deliver_message(make_hash(1), make_hash(1));
        assert_eq!(cache.min_expire_at.load(Ordering::Acquire), 10);

        cache.update(&ton_block::ShardIdent::masterchain(), 15);
        assert_eq!(cache.min_expire_at.load(Ordering::Acquire), u32::MAX);

        assert_eq!(rx1.await.unwrap(), MessageStatus::Expired);
        assert_eq!(rx2.await.unwrap(), MessageStatus::Delivered);

        // Add messages
        let rx1 = cache.add_message(make_hash(0), make_hash(0), 10).unwrap();
        let rx2 = cache.add_message(make_hash(1), make_hash(1), 20).unwrap();

        cache.deliver_message(make_hash(0), make_hash(0));
        assert_eq!(cache.min_expire_at.load(Ordering::Acquire), 20);

        cache.deliver_message(make_hash(1), make_hash(1));
        assert_eq!(cache.min_expire_at.load(Ordering::Acquire), u32::MAX);

        assert_eq!(rx1.await.unwrap(), MessageStatus::Delivered);
        assert_eq!(rx2.await.unwrap(), MessageStatus::Delivered);
    }
}
