use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use rocksdb::perf::MemoryUsageStats;
use rocksdb::DBCompressionType;

use self::utxo_balance::UtxoBalancesStorage;

use rocksdb_builder::DbCaches;

mod columns;
mod migrations;
mod utxo_balance;

pub struct Db {
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
    pub async fn new<PS>(rocksdb_path: PS, mem_limit: usize) -> Result<Arc<Self>>
    where
        PS: AsRef<Path>,
    {
        let limit = 256; // TODO: CHECK LIMIT

        let caches = DbCaches::with_capacity(mem_limit)?;

        let db = rocksdb_builder::DbBuilder::new(rocksdb_path, &caches)
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
            .column::<columns::UtxoBalances>()
            .build()
            .context("Failed building db")?;

        migrations::apply(&db)
            .await
            .context("Failed to apply migrations")?;

        let utxo_balance_storage = Arc::new(UtxoBalancesStorage::with_db(&db)?);

        Ok(Arc::new(Self {
            utxo_balance_storage,
            db,
            caches,
        }))
    }

    #[inline(always)]
    pub fn utxo_balance_storage(&self) -> &UtxoBalancesStorage {
        self.utxo_balance_storage.as_ref()
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
