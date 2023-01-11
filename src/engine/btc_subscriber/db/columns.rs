use rocksdb::{BlockBasedOptions, Options};
use rocksdb_builder::{Column, DbCaches};

/// Maps UTXOs to amount
/// - Key: `bitcoin::hash_types::Txid`
/// - Value: `u64`
pub struct UtxoBalances;
impl Column for UtxoBalances {
    const NAME: &'static str = "utxo_balances";

    fn options(opts: &mut Options, caches: &DbCaches) {
        default_block_based_table_factory(opts, caches); // TODO: check options for column family
    }
}

fn default_block_based_table_factory(opts: &mut Options, caches: &DbCaches) {
    let mut block_factory = BlockBasedOptions::default();
    block_factory.set_block_cache(&caches.block_cache);
    block_factory.set_block_cache_compressed(&caches.compressed_block_cache);
    opts.set_block_based_table_factory(&block_factory);
}
