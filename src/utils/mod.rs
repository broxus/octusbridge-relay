use std::borrow::Borrow;
use std::hash::BuildHasherDefault;

use dashmap::DashMap;
use rustc_hash::FxHasher;

pub use self::evm_address::*;
pub use self::existing_contract::*;
pub use self::memory_cache::*;
pub use self::pending_messages_queue::*;
pub use self::retry::*;
pub use self::serde_helpers::*;
pub use self::shard_utils::*;
pub use self::tristate::*;
pub use self::tx_context::*;

mod evm_address;
mod existing_contract;
mod memory_cache;
mod pending_messages_queue;
mod retry;
mod serde_helpers;
mod shard_utils;
mod tristate;
mod tx_context;

pub const LEGACY_ABI_VERSION: ton_abi::contract::AbiVersion = ton_abi::contract::ABI_VERSION_2_2;
pub const LATEST_ABI_VERSION: ton_abi::contract::AbiVersion = ton_abi::contract::ABI_VERSION_2_7;

#[macro_export]
macro_rules! once {
    ($ty:path, || $expr:expr) => {{
        static ONCE: once_cell::race::OnceBox<$ty> = once_cell::race::OnceBox::new();
        ONCE.get_or_init(|| Box::new($expr))
    }};
}

pub type FxDashMap<K, V> = DashMap<K, V, BuildHasherDefault<FxHasher>>;

#[derive(Clone, Copy)]
pub struct DisplayAddr<T>(pub T);

impl<T> std::fmt::Display for DisplayAddr<T>
where
    T: Borrow<ton_types::UInt256>,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("0:{:x}", self.0.borrow()))
    }
}

#[derive(Clone, Copy)]
pub struct DisplayCodeHash<T>(pub T);

impl<T> std::fmt::Display for DisplayCodeHash<T>
where
    T: Borrow<ton_types::UInt256>,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("{:x}", self.0.borrow()))
    }
}

fn is_sorted_desc<EL, PROP: Ord>(arr: &[EL], by: fn(&EL) -> PROP) -> bool {
    arr.windows(2).all(|w| by(&w[0]) >= by(&w[1]))
}

pub fn sort_maybe_desc_to_asc<EL, PROP: Ord>(arr: &mut [EL], by: fn(&EL) -> PROP) {
    if is_sorted_desc(arr, by) {
        arr.reverse();
    } else {
        arr.sort_by_key(by);
    }
}

#[cfg(test)]
mod tests {
    use super::is_sorted_desc;
    use super::sort_maybe_desc_to_asc;

    #[test]
    fn test_sort_maybe_desc_to_asc() {
        #[derive(Eq, PartialEq, Hash, Debug)]
        struct W(i32);

        let mut v = vec![W(5), W(4), W(3), W(2), W(1)];
        assert!(is_sorted_desc(&v, |w| w.0));
        sort_maybe_desc_to_asc(&mut v, |w| w.0);
        assert_eq!(v, vec![W(1), W(2), W(3), W(4), W(5)]);

        let mut v = vec![W(4), W(5), W(3), W(2), W(1)];
        assert!(!is_sorted_desc(&v, |w| w.0));
        sort_maybe_desc_to_asc(&mut v, |w| w.0);
        assert_eq!(v, vec![W(1), W(2), W(3), W(4), W(5)]);
    }
}
