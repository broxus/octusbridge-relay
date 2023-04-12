use nekoton_abi::*;

use super::models::*;

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

/// External function
pub fn finalize() -> &'static ton_abi::Function {
    crate::once!(ton_abi::Function, || {
        FunctionBuilder::new("finalize")
            .abi_version(ton_abi::contract::ABI_VERSION_2_2)
            .time_header()
            .build()
    })
}

/// External responsible function
pub fn get_event_init_data() -> &'static ton_abi::Function {
    crate::once!(ton_abi::Function, || {
        FunctionBuilder::new_responsible("getDetails")
            .abi_version(ton_abi::contract::ABI_VERSION_2_2)
            .time_header()
            .output("details", TonBtcEventInitData::param_type())
            .build()
    })
}

/// External responsible function
pub fn get_transactions() -> &'static ton_abi::Function {
    crate::once!(ton_abi::Function, || {
        FunctionBuilder::new("transactions")
            .abi_version(ton_abi::contract::ABI_VERSION_2_2)
            .time_header()
            .output("transactions", TonBtcTransactions::param_type())
            .build()
    })
}

pub mod events {
    use super::*;
    use crate::engine::ton_contracts::BtcWithdrawal;

    pub fn add_withdrawal() -> &'static ton_abi::Event {
        crate::once!(ton_abi::Event, || {
            EventBuilder::new("AddWithdrawal")
                .abi_version(ton_abi::contract::ABI_VERSION_2_2)
                .input("withdrawal", BtcWithdrawal::param_type())
                .build()
        })
    }
}
