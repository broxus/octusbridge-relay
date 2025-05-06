use nekoton_abi::*;

use super::models::*;

const ABI_VERSION: ton_abi::contract::AbiVersion = super::CONTRACTS_ABI_VERSION;

/// External function
pub fn get_details() -> &'static ton_abi::Function {
    crate::once!(ton_abi::Function, || {
        FunctionBuilder::new_responsible("getDetails")
            .abi_version(ABI_VERSION)
            .time_header()
            .expire_header()
            .outputs(TvmSvmEventConfigurationDetails::param_type())
            .build()
    })
}

/// Internal function
pub fn set_end_timestamp() -> &'static ton_abi::Function {
    crate::once!(ton_abi::Function, || {
        FunctionBuilder::new("setEndTimestamp")
            .abi_version(ABI_VERSION)
            .input("end_timestamp", ton_abi::ParamType::Uint(32))
            .build()
    })
}
