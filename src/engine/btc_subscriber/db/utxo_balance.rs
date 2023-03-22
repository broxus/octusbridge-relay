use std::sync::Arc;

use anyhow::{Context, Result};
use rocksdb_builder::Tree;

use super::columns;

pub struct UtxoBalancesStorage {
    balances: Tree<columns::UtxoBalances>,
}

impl UtxoBalancesStorage {
    pub fn with_db(db: &Arc<rocksdb::DB>) -> Result<Self> {
        Ok(Self {
            balances: Tree::new(db)?,
        })
    }

    pub async fn store_balance(&self, id: bitcoin::Script, balance: u64) -> Result<()> {
        self.balances.insert(id, balance.to_le_bytes())?;
        Ok(())
    }

    pub async fn remove_balance(&self, id: bitcoin::Script) -> Result<()> {
        self.balances.remove(id)?;
        Ok(())
    }

    pub fn balances_iterator(&self) -> impl Iterator<Item = (bitcoin::Script, u64)> + '_ {
        let raw_iterator = self.balances.raw_iterator();
        UtxoBalancesIterator { raw_iterator }
    }
}

struct UtxoBalancesIterator<'a> {
    raw_iterator: rocksdb::DBRawIterator<'a>,
}

impl Iterator for UtxoBalancesIterator<'_> {
    type Item = (bitcoin::Script, u64);

    fn next(&mut self) -> Option<Self::Item> {
        let value = self.raw_iterator.item().map(|(k, v)| {
            let balance = v
                .try_into()
                .context("can not parse balance")
                .map(u64::from_le_bytes);

            let script = bitcoin::PublicKey::from_slice(k)
                .context("can not parse script")
                .map(|pk| bitcoin::Script::new_p2pk(&pk));

            (script, balance)
        })?;
        self.raw_iterator.next();

        match value {
            (Ok(script), Ok(balance)) => Some((script, balance)),
            t => {
                tracing::error!(
                    "Can not parse UTXO balance from raw iterator - {:#?}, from script - {:?}",
                    t.1,
                    t.0
                );
                None
            }
        }
    }
}
