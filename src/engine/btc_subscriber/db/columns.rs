use super::{Column};

/// Maps UTXOs to amount
/// - Key: `bitcoin::hash_types::Txid`
/// - Value: `u64`
pub struct UtxoBalance;
impl Column for UtxoBalance {
    const NAME: &'static str = "utxo_balances";
}

