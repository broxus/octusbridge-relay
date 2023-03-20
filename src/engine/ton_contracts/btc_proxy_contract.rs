use nekoton_abi::*;

/// External responsible function
pub fn get_deposit_address() -> &'static ton_abi::Function {
    crate::once!(ton_abi::Function, || {
        FunctionBuilder::new_responsible("deriveDepositAccount")
            .abi_version(ton_abi::contract::ABI_VERSION_2_2)
            .time_header()
            .input("beneficiary", ton_abi::ParamType::Address)
            .output("account", ton_abi::ParamType::Address)
            .build()
    })
}
