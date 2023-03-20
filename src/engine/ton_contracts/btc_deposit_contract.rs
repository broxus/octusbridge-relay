use nekoton_abi::*;

/// External responsible function
pub fn id() -> &'static ton_abi::Function {
    crate::once!(ton_abi::Function, || {
        FunctionBuilder::new("id")
            .abi_version(ton_abi::contract::ABI_VERSION_2_2)
            .time_header()
            .output("id", ton_abi::ParamType::Uint(256))
            .build()
    })
}
