use std::collections::HashMap;
use std::sync::{Arc};

use tokio::sync::Mutex;
use anyhow::Result;

use super::{columns, Tree};
use crate::utils::*;

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
        self.balances
            .insert(id, balance)?;
        Ok(())
    }

    pub fn balances_iterator(
        &self,
    ) -> impl Iterator<Item = Result<( bitcoin::hash_types::Txid, u64)>> + '_ {
        let mut raw_iterator = self.balances.raw_iterator();

        UtxoBalancesIterator {
            raw_iterator,
        }
    }
}

struct UtxoBalancesIterator<'a> {
    raw_iterator: rocksdb::DBRawIterator<'a>,
}

impl Iterator for UtxoBalancesIterator<'_> {
    type Item = Result<u64>;

    fn next(&mut self) -> Option<Self::Item> {
        let value = self
            .raw_iterator
            .value()
            .map(u64)?;
        self.raw_iterator.next();

        Some(value)
    }
}
