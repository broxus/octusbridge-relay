use nekoton_abi::*;
use once_cell::sync::OnceCell;

use super::models::*;

pub fn get_details() -> &'static ton_abi::Function {
    static FUNCTION: OnceCell<ton_abi::Function> = OnceCell::new();
    FUNCTION.get_or_init(|| {
        FunctionBuilder::new("getDetails")
            .time_header()
            .outputs(ConnectorDetails::param_type())
            .build()
    })
}

pub mod events {
    use super::*;

    pub fn enabled() -> &'static ton_abi::Event {
        static EVENT: OnceCell<ton_abi::Event> = OnceCell::new();
        EVENT.get_or_init(|| EventBuilder::new("Enabled").build())
    }
}