use nekoton_abi::*;

const ABI_VERSION: ton_abi::contract::AbiVersion = super::CONTRACTS_ABI_VERSION;

/// External responsible function
pub fn wallet_of() -> &'static ton_abi::Function {
    crate::once!(ton_abi::Function, || {
        FunctionBuilder::new_responsible("walletOf")
            .abi_version(ABI_VERSION)
            .default_headers()
            .input("walletOwner", ton_abi::ParamType::Address)
            .output("value0", ton_abi::ParamType::Address)
            .build()
    })
}

/// External responsible function.
pub fn total_supply() -> &'static ton_abi::Function {
    crate::once!(ton_abi::Function, || {
        FunctionBuilder::new_responsible("totalSupply")
            .abi_version(ABI_VERSION)
            .default_headers()
            .output("totalSupply", ton_abi::ParamType::Uint(128))
            .build()
    })
}

/// External responsible function.
pub fn root_owner() -> &'static ton_abi::Function {
    crate::once!(ton_abi::Function, || {
        FunctionBuilder::new_responsible("rootOwner")
            .abi_version(ABI_VERSION)
            .default_headers()
            .output("value0", ton_abi::ParamType::Address)
            .build()
    })
}
