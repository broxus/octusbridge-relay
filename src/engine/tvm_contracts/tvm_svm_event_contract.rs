use nekoton_abi::*;

use super::models::*;

const ABI_VERSION: ton_abi::contract::AbiVersion = super::CONTRACTS_ABI_VERSION;

/// External responsible function
pub fn get_event_init_data() -> &'static ton_abi::Function {
    crate::once!(ton_abi::Function, || {
        FunctionBuilder::new_responsible("getEventInitData")
            .abi_version(ABI_VERSION)
            .default_headers()
            .output("details", TvmSvmEventInitData::param_type())
            .build()
    })
}

/// External function
pub fn created_at() -> &'static ton_abi::Function {
    crate::once!(ton_abi::Function, || {
        FunctionBuilder::new("createdAt")
            .abi_version(ABI_VERSION)
            .default_headers()
            .output("createdAt", u32::param_type())
            .build()
    })
}

/// External function
pub fn confirm() -> &'static ton_abi::Function {
    crate::once!(ton_abi::Function, || {
        FunctionBuilder::new("confirm")
            .abi_version(ABI_VERSION)
            .default_headers()
            .input("voteReceiver", ton_abi::ParamType::Address)
            .build()
    })
}

/// External function
pub fn reject() -> &'static ton_abi::Function {
    crate::once!(ton_abi::Function, || {
        FunctionBuilder::new("reject")
            .abi_version(ABI_VERSION)
            .default_headers()
            .input("voteReceiver", ton_abi::ParamType::Address)
            .build()
    })
}
