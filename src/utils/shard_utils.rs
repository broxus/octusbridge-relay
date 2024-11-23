use std::borrow::Borrow;

use ton_types::UInt256;

pub fn only_account_hash<T>(address: T) -> UInt256
where
    T: Borrow<ton_block::MsgAddressInt>,
{
    UInt256::from_be_bytes(&address.borrow().address().get_bytestring(0))
}
