use super::models::*;
use nekoton_abi::*;

const ABI_VERSION: ton_abi::contract::AbiVersion = if cfg!(feature = "legacy-bridge-contract") {
    crate::utils::LEGACY_ABI_VERSION
} else {
    super::CONTRACTS_ABI_VERSION
};

/// External function
pub fn connector_counter() -> &'static ton_abi::Function {
    crate::once!(ton_abi::Function, || {
        FunctionBuilder::new("connectorCounter")
            .abi_version(ABI_VERSION)
            .default_headers()
            .output("counter", ton_abi::ParamType::Uint(64))
            .build()
    })
}

#[cfg(not(feature = "disable-staking"))]
/// External responsible function
pub fn get_details() -> &'static ton_abi::Function {
    crate::once!(ton_abi::Function, || {
        FunctionBuilder::new_responsible("getDetails")
            .abi_version(ABI_VERSION)
            .default_headers()
            .outputs(BridgeDetails::param_type())
            .build()
    })
}

/// External function
pub fn derive_connector_address() -> &'static ton_abi::Function {
    crate::once!(ton_abi::Function, || {
        FunctionBuilder::new("deriveConnectorAddress")
            .abi_version(ABI_VERSION)
            .default_headers()
            .input("id", ton_abi::ParamType::Uint(64))
            .output("connector", ton_abi::ParamType::Address)
            .build()
    })
}

pub mod events {
    use super::*;

    pub fn connector_deployed() -> &'static ton_abi::Event {
        crate::once!(ton_abi::Event, || {
            EventBuilder::new("ConnectorDeployed")
                .abi_version(ABI_VERSION)
                .inputs(ConnectorDeployedEvent::param_type())
                .build()
        })
    }
}
