use ton_abi::{ParamType, TokenValue};
use ton_block::MsgAddressInt;
use ton_types::UInt256;

#[test]
fn nekoton_default_address_test() {
    use nekoton_abi::{UnpackAbi, UnpackerResult};

    let unpacked_address: UnpackerResult<MsgAddressInt> =
        UnpackAbi::unpack(TokenValue::Address(Default::default()));

    assert!(unpacked_address.is_ok());
}

#[test]
fn nekoton_default_address_only_hash_test() {
    use nekoton_abi::{UnpackerResult, address_only_hash};

    let unpacked_address: UnpackerResult<UInt256> =
        address_only_hash::unpack(&TokenValue::Address(Default::default()));

    assert!(unpacked_address.is_ok());
}

#[test]
fn nekoton_array_default_address_only_hash_test() {
    use nekoton_abi::{UnpackerResult, array_address_only_hash};

    let unpacked_address: UnpackerResult<Vec<UInt256>> =
        array_address_only_hash::unpack(&TokenValue::Array(
            ParamType::Address,
            vec![TokenValue::Address(Default::default())],
        ));

    assert!(unpacked_address.is_ok());
}
