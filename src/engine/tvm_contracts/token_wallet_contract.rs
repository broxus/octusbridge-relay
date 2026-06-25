use nekoton_abi::*;

/// Token root address
pub fn root() -> &'static ton_abi::Function {
    crate::once!(ton_abi::Function, || {
        FunctionBuilder::new_responsible("root")
            .abi_version(ton_abi::contract::ABI_VERSION_2_2)
            .default_headers()
            .output("value0", ton_abi::ParamType::Address)
            .build()
    })
}
