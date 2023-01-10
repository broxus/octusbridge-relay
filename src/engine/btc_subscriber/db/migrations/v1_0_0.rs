use std::sync::Arc;

use anyhow::{Context, Result};

use super::Migrations;
use crate::utils::*;

// 1.0.0
pub(super) fn register(migrations: &mut Migrations) -> Result<()> {
    migrations.register([0,0,0],[1, 0, 0], |db| async move {
        new_utxos_columns(&db)?;
        Ok(())
    })
}

fn new_utxos_columns(db: &Arc<rocksdb::DB>) -> Result<()> {
    // TODO: !! create column
    Ok(())
}
