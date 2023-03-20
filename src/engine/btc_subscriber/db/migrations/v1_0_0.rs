use std::sync::Arc;

use anyhow::Result;
use rocksdb_builder::{Column, DbCaches, Tree};

use super::super::columns;
use super::Migrations;

// 1.0.0
pub(super) fn register(migrations: &mut Migrations) -> Result<()> {
    migrations.register([0, 0, 0], [1, 0, 0], |db| async move {
        new_utxo_column(&db)?;
        Ok(())
    })
}

fn new_utxo_column(db: &Arc<rocksdb::DB>) -> Result<()> {
    if Tree::<columns::UtxoBalances>::new(db).is_err() {
        let mut options = Default::default();
        let caches = DbCaches::with_capacity(DbCaches::MIN_CAPACITY)?;
        columns::UtxoBalances::options(&mut options, &caches);
        db.create_cf(columns::UtxoBalances::NAME, &options)?;
    }
    Ok(())
}
