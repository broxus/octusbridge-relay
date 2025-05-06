use nekoton_abi::*;

use super::models::*;

const ABI_VERSION: ton_abi::contract::AbiVersion = super::CONTRACTS_ABI_VERSION;

pub fn staker_addrs() -> &'static ton_abi::Function {
    crate::once!(ton_abi::Function, || {
        FunctionBuilder::new("staker_addrs")
            .abi_version(ABI_VERSION)
            .time_header()
            .expire_header()
            .outputs(StakerAddresses::param_type())
            .build()
    })
}
