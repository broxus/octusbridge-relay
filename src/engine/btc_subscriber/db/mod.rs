use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use rocksdb::perf::MemoryUsageStats;
use rocksdb::DBCompressionType;

use crate::utils::*;
use self::tree::*;
use self::utxo_balance::UtxoBalancesStorage;

mod utxo_balance;
mod columns;
mod migrations;
mod tree;

pub struct Db {
    file_db_path: PathBuf,
    utxo_balance_storage: Arc<UtxoBalancesStorage>,
    db: Arc<rocksdb::DB>,
    caches: DbCaches,
}

pub struct RocksdbStats {
    pub whole_db_stats: MemoryUsageStats,
    pub uncompressed_block_cache_usage: usize,
    pub uncompressed_block_cache_pined_usage: usize,
    pub compressed_block_cache_usage: usize,
    pub compressed_block_cache_pined_usage: usize,
}

impl Db {
    pub async fn new<PS, PF>(
        rocksdb_path: PS,
        file_db_path: PF,
        mem_limit: usize,
    ) -> Result<Arc<Self>>
    where
        PS: AsRef<Path>,
        PF: AsRef<Path>,
    {
        let limit = 256; // TODO: CHECK LIMIT

        let caches = DbCaches::with_capacity(mem_limit)?;

        let db = DbBuilder::new(rocksdb_path, &caches)
            .options(|opts, _| {
                opts.set_level_compaction_dynamic_level_bytes(true);

                // compression opts
                opts.set_zstd_max_train_bytes(32 * 1024 * 1024);
                opts.set_compression_type(DBCompressionType::Zstd);

                // io
                opts.set_max_open_files(limit as i32);

                // logging
                opts.set_log_level(rocksdb::LogLevel::Error);
                opts.set_keep_log_file_num(2);
                opts.set_recycle_log_file_num(2);

                // cf
                opts.create_if_missing(true);
                opts.create_missing_column_families(true);

                // cpu
                opts.set_max_background_jobs(std::cmp::max((num_cpus::get() as i32) / 2, 2));
                opts.increase_parallelism(num_cpus::get() as i32);

                // debug
                // opts.enable_statistics();
                // opts.set_stats_dump_period_sec(30);
            })
            .column::<columns::UtxoBalance>()
            .build()
            .context("Failed building db")?;

        migrations::apply(&db)
            .await
            .context("Failed to apply migrations")?;

        let utxo_balance_storage = Arc::new(UtxoBalancesStorage::with_db(&db)?);

        Ok(Arc::new(Self {
            file_db_path: file_db_path.as_ref().to_path_buf(),
            utxo_balance_storage,
            db,
            caches,
        }))
    }

    #[inline(always)]
    pub fn file_db_path(&self) -> &Path {
        &self.file_db_path
    }

    #[inline(always)]
    pub fn runtime_storage(&self) -> &RuntimeStorage {
        self.runtime_storage.as_ref()
    }

    #[inline(always)]
    pub fn block_handle_storage(&self) -> &BlockHandleStorage {
        self.block_handle_storage.as_ref()
    }

    #[inline(always)]
    pub fn block_connection_storage(&self) -> &BlockConnectionStorage {
        &self.block_connection_storage
    }

    #[inline(always)]
    pub fn block_storage(&self) -> &BlockStorage {
        self.block_storage.as_ref()
    }

    #[inline(always)]
    pub fn shard_state_storage(&self) -> &ShardStateStorage {
        &self.shard_state_storage
    }

    #[inline(always)]
    pub fn node_state(&self) -> &NodeStateStorage {
        &self.node_state_storage
    }

    pub fn metrics(&self) -> DbMetrics {
        DbMetrics {
            shard_state_storage: self.shard_state_storage.metrics(),
        }
    }

    pub fn get_memory_usage_stats(&self) -> Result<RocksdbStats> {
        let caches = &[
            &self.caches.block_cache,
            &self.caches.compressed_block_cache,
        ];
        let whole_db_stats =
            rocksdb::perf::get_memory_usage_stats(Some(&[&self.db]), Some(caches))?;

        let uncompressed_block_cache_usage = self.caches.block_cache.get_usage();
        let uncompressed_block_cache_pined_usage = self.caches.block_cache.get_pinned_usage();

        let compressed_block_cache_usage = self.caches.compressed_block_cache.get_usage();
        let compressed_block_cache_pined_usage =
            self.caches.compressed_block_cache.get_pinned_usage();

        Ok(RocksdbStats {
            whole_db_stats,
            uncompressed_block_cache_usage,
            uncompressed_block_cache_pined_usage,
            compressed_block_cache_usage,
            compressed_block_cache_pined_usage,
        })
    }
}

#[derive(Debug, Copy, Clone)]
pub struct DbMetrics {
    pub shard_state_storage: ShardStateStorageMetrics,
}
