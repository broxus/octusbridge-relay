use std::collections::hash_map;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Result;
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use tokio::sync::oneshot;
use ton_types::UInt256;

pub struct PendingMessagesQueue {
    min_expire_at: AtomicU32,
    entries: Mutex<FxHashMap<PendingMessageId, PendingMessage>>,
    entry_count: AtomicUsize,
}

impl PendingMessagesQueue {
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            min_expire_at: AtomicU32::new(u32::MAX),
            entries: Mutex::new(FxHashMap::with_capacity_and_hasher(
                capacity,
                Default::default(),
            )),
            entry_count: Default::default(),
        })
    }

    pub fn len(&self) -> usize {
        self.entry_count.load(Ordering::Acquire)
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
                self.entry_count.fetch_add(1, Ordering::Release);

                Ok(rx)
            }
            hash_map::Entry::Occupied(_) => Err(PendingMessagesQueueError::AlreadyExists.into()),
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

        self.entry_count.fetch_sub(1, Ordering::Release);

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
enum PendingMessagesQueueError {
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

    fn make_queue() -> Arc<PendingMessagesQueue> {
        let queue = PendingMessagesQueue::new(10);
        assert_eq!(queue.min_expire_at.load(Ordering::Acquire), u32::MAX);
        queue
    }

    #[tokio::test]
    async fn normal_message_flow() {
        let queue = make_queue();

        // Add message
        let rx = queue.add_message(make_hash(0), make_hash(0), 10).unwrap();

        // (Adding same message should fail)
        assert!(queue.add_message(make_hash(0), make_hash(0), 20).is_err());
        // Adding new message must update expiration
        assert_eq!(queue.min_expire_at.load(Ordering::Acquire), 10);

        // Deliver message
        queue.deliver_message(make_hash(0), make_hash(0));
        assert_eq!(queue.min_expire_at.load(Ordering::Acquire), u32::MAX);
        assert_eq!(rx.await.unwrap(), MessageStatus::Delivered);
    }
}
