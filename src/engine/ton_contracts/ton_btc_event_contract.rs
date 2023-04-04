use nekoton_abi::*;

/// External function
pub fn confirm() -> &'static ton_abi::Function {
    crate::once!(ton_abi::Function, || {
        FunctionBuilder::new("confirm")
            .abi_version(ton_abi::contract::ABI_VERSION_2_2)
            .time_header()
            .input("transaction", ton_abi::ParamType::Bytes)
            .input("voteReceiver", ton_abi::ParamType::Address)
            .build()
    })
}

/// External function
pub fn reject() -> &'static ton_abi::Function {
    crate::once!(ton_abi::Function, || {
        FunctionBuilder::new("reject")
            .abi_version(ton_abi::contract::ABI_VERSION_2_2)
            .time_header()
            .input("voteReceiver", ton_abi::ParamType::Address)
            .build()
    })
}

/// External function
pub fn cancel() -> &'static ton_abi::Function {
    crate::once!(ton_abi::Function, || {
        FunctionBuilder::new("cancel")
            .abi_version(ton_abi::contract::ABI_VERSION_2_2)
            .time_header()
            .build()
    })
}

pub mod events {
    use super::*;

    pub fn add_withdrawal() -> &'static ton_abi::Event {
        crate::once!(ton_abi::Event, || {
            EventBuilder::new("AddWithdrawal")
                .abi_version(ton_abi::contract::ABI_VERSION_2_2)
                .build()
        })
    }
}
