use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use bitcoin::hashes::{sha256d, Hash};
use rocksdb_builder::Tree;
use tokio::sync::Mutex;
use tracing::log;

use super::columns;

pub struct UtxoBalancesStorage {
    cache: Arc<Mutex<HashMap<bitcoin::hash_types::Txid, u64>>>,
    balances: Tree<columns::UtxoBalance>,
}

impl UtxoBalancesStorage {
    pub fn with_db(db: &Arc<rocksdb::DB>) -> Result<Self> {
        Ok(Self {
            cache: Arc::new(Mutex::new(Default::default())),
            balances: Tree::new(db)?,
        })
    }

    pub async fn store_balance(&self, id: bitcoin::hash_types::Txid, balance: u64) -> Result<()> {
        let mut hash = self.cache.lock().await;
        hash.insert(id, balance);
        self.balances.insert(id, balance.to_le_bytes())?;
        Ok(())
    }

    pub fn balances_iterator(&self) -> impl Iterator<Item = (bitcoin::hash_types::Txid, u64)> + '_ {
        let raw_iterator = self.balances.raw_iterator();

        UtxoBalancesIterator { raw_iterator }
    }
}

struct UtxoBalancesIterator<'a> {
    raw_iterator: rocksdb::DBRawIterator<'a>,
}

impl Iterator for UtxoBalancesIterator<'_> {
    type Item = (bitcoin::hash_types::Txid, u64);

    fn next(&mut self) -> Option<Self::Item> {
        let value = self.raw_iterator.item().map(|(k, v)| {
            let balance = v
                .try_into()
                .context("can not parse balance")
                .map(u64::from_le_bytes);

            let tx_id = sha256d::Hash::from_slice(k)
                .context("can not parse tx id")
                .map(bitcoin::hash_types::Txid::from_hash);
            (tx_id, balance)
        })?;
        self.raw_iterator.next();

        match value {
            (Ok(tx), Ok(balance)) => Some((tx, balance)),
            t => {
                log::error!(
                    "Can not parse UTXO balance from raw iterator - {:#?}, from tx id - {:?}",
                    t.1,
                    1.0
                );
                None
            }
        }
    }
}
