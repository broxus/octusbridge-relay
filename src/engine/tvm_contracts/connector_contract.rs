use nekoton_abi::*;

use super::{models::*, CONFIGURABLE_ABI_VERSION};

pub fn get_details() -> &'static ton_abi::Function {
    crate::once!(ton_abi::Function, || {
        FunctionBuilder::new("getDetails")
            .abi_version(CONFIGURABLE_ABI_VERSION)
            .time_header()
            .outputs(ConnectorDetails::param_type())
            .build()
    })
}

pub mod events {
    use super::*;

    pub fn enabled() -> &'static ton_abi::Event {
        crate::once!(ton_abi::Event, || EventBuilder::new("Enabled")
            .abi_version(CONFIGURABLE_ABI_VERSION)
            .build())
    }
}
