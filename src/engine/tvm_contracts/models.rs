use nekoton_abi::*;
use ton_block::MsgAddressInt;
use ton_types::{Cell, UInt256};

#[derive(Debug, Clone, PackAbi, UnpackAbi, KnownParamType)]
pub struct EvmTvmEventInitData {
    #[abi]
    pub vote_data: EvmTvmEventVoteData,
    #[abi(with = "address_only_hash")]
    pub configuration: UInt256,
    #[abi(with = "address_only_hash")]
    pub staking: UInt256,
    #[abi(uint32)]
    pub chain_id: u32,
}

#[derive(Debug, Clone, PackAbi, UnpackAbi, KnownParamType)]
pub struct EvmTvmEventVoteData {
    #[abi(uint256)]
    pub event_transaction: UInt256,
    #[abi(uint32)]
    pub event_index: u32,
    #[abi(cell)]
    pub event_data: Cell,
    #[abi(uint32)]
    pub event_block_number: u32,
    #[abi(uint256)]
    pub event_block: UInt256,
}

#[derive(Debug, Clone, PackAbi, UnpackAbiPlain, KnownParamTypePlain)]
pub struct EvmTvmEventDecodedData {
    #[abi(with = "address_only_hash")]
    pub token: UInt256,
    #[abi]
    pub amount: u128,
    #[abi(with = "address_only_hash")]
    pub recipient: UInt256,
    #[abi]
    pub value: UInt256,
    #[abi]
    pub expected_gas: UInt256,
    #[abi]
    pub payload: Cell,
    #[abi]
    pub proxy: MsgAddressInt,
    #[abi]
    pub token_wallet: MsgAddressInt,
}

#[derive(Debug, Clone, PackAbi, UnpackAbi, KnownParamType)]
pub struct TvmEvmEventInitData {
    #[abi]
    pub vote_data: TvmEvmEventVoteData,
    #[abi(with = "address_only_hash")]
    pub configuration: UInt256,
    #[abi(with = "address_only_hash")]
    pub staking: UInt256,
}

#[derive(Debug, Clone, PackAbi, UnpackAbi, KnownParamType)]
pub struct TvmEvmEventVoteData {
    #[abi(uint64)]
    pub event_transaction_lt: u64,
    #[abi(uint32)]
    pub event_timestamp: u32,
    #[abi(cell)]
    pub event_data: Cell,
}

#[derive(Debug, Clone, PackAbi, UnpackAbiPlain, KnownParamTypePlain)]
pub struct TvmEvmEventDecodedData {
    #[abi(name = "proxy_")]
    pub proxy: MsgAddressInt,
    #[abi(name = "tokenWallet_")]
    pub token_wallet: MsgAddressInt,
    #[abi(name = "token_")]
    pub token: MsgAddressInt,
    #[abi(name = "remainingGasTo_")]
    pub remaining_gas_to: MsgAddressInt,
    #[abi(name = "amount_")]
    pub amount: u128,
    #[abi(name = "recipient_", with = "uint160_bytes")]
    pub recipient: [u8; 20],
    #[abi(name = "chainId_")]
    pub chain_id: UInt256,
    #[abi]
    pub callback: TvmEvmEventDecodedDataCallback,
    #[abi(name = "name_", string)]
    pub name: String,
    #[abi(name = "symbol_", string)]
    pub symbol: String,
    #[abi(name = "decimals_")]
    pub decimals: u8,
}

#[derive(Debug, Clone, UnpackAbi, PackAbi, KnownParamType)]
pub struct TvmEvmEventDecodedDataCallback {
    #[abi(with = "uint160_bytes")]
    pub recipient: [u8; 20],
    #[abi]
    pub payload: Vec<u8>,
    #[abi]
    pub strict: bool,
}

#[derive(Debug, Clone, PackAbi, UnpackAbi, KnownParamType)]
pub struct SvmTvmEventInitData {
    #[abi]
    pub vote_data: SvmTvmEventVoteData,
    #[abi(with = "address_only_hash")]
    pub configuration: UInt256,
    #[abi(with = "address_only_hash")]
    pub staking: UInt256,
}

#[derive(Debug, Clone, PackAbi, UnpackAbi, KnownParamType)]
pub struct SvmTvmEventVoteData {
    #[abi(uint128)]
    pub account_seed: u128,
    #[abi(uint64)]
    pub slot: u64,
    #[abi(uint64)]
    pub block_time: u64,
    #[abi(string)]
    pub signature: String,
    #[abi(cell)]
    pub event_data: Cell,
}

#[derive(Debug, Clone, PackAbi, UnpackAbi, KnownParamType)]
pub struct TvmSvmEventInitData {
    #[abi]
    pub vote_data: TvmSvmEventVoteData,
    #[abi(with = "address_only_hash")]
    pub configuration: UInt256,
    #[abi(with = "address_only_hash")]
    pub staking: UInt256,
}

#[derive(Debug, Clone, PackAbi, UnpackAbi, KnownParamType)]
pub struct TvmSvmEventVoteData {
    #[abi(uint64)]
    pub event_transaction_lt: u64,
    #[abi(uint32)]
    pub event_timestamp: u32,
    #[abi(array)]
    pub execute_accounts: Vec<ExecuteAccount>,
    #[abi(bool)]
    pub execute_payload_needed: bool,
    #[abi(array)]
    pub execute_payload_accounts: Vec<ExecuteAccount>,
    #[abi(cell)]
    pub event_data: Cell,
}

#[derive(Debug, Clone, PackAbi, UnpackAbi, KnownParamType)]
pub struct ExecuteAccount {
    #[abi(uint256)]
    pub account: UInt256,
    #[abi(bool)]
    pub read_only: bool,
    #[abi(bool)]
    pub is_signer: bool,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, PackAbi, UnpackAbi, KnownParamType)]
pub enum EventStatus {
    Initializing = 0,
    Pending = 1,
    Confirmed = 2,
    Rejected = 3,
    Cancelled = 4,
    LimitReached = 5,
    LiquidityRequested = 6,
    LiquidityProvided = 7,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, PackAbi, UnpackAbi, KnownParamType)]
pub enum EventVote {
    Reserved = 0,
    Empty = 1,
    Confirm = 2,
    Reject = 3,
}

#[derive(Debug, Clone, PackAbiPlain, UnpackAbiPlain, KnownParamTypePlain)]
pub struct EvmTvmEventConfigurationDetails {
    #[abi]
    pub basic_configuration: BasicConfiguration,
    #[abi]
    pub network_configuration: EvmTvmEventConfiguration,
    #[abi(cell)]
    pub meta: Cell,
}

#[derive(Debug, Clone, PackAbiPlain, UnpackAbiPlain, KnownParamTypePlain)]
pub struct TvmEvmEventConfigurationDetails {
    #[abi]
    pub basic_configuration: BasicConfiguration,
    #[abi]
    pub network_configuration: TvmEvmEventConfiguration,
    #[abi(cell)]
    pub meta: Cell,
}

#[derive(Debug, Clone, PackAbiPlain, UnpackAbiPlain, KnownParamTypePlain)]
pub struct SvmTvmEventConfigurationDetails {
    #[abi]
    pub basic_configuration: BasicConfiguration,
    #[abi]
    pub network_configuration: SvmTvmEventConfiguration,
    #[abi(cell)]
    pub meta: Cell,
}

#[derive(Debug, Clone, PackAbiPlain, UnpackAbiPlain, KnownParamTypePlain)]
pub struct TvmSvmEventConfigurationDetails {
    #[abi]
    pub basic_configuration: BasicConfiguration,
    #[abi]
    pub network_configuration: TvmSvmEventConfiguration,
    #[abi(cell)]
    pub meta: Cell,
}

#[derive(Debug, Clone, PackAbi, UnpackAbi, KnownParamType)]
pub struct BasicConfiguration {
    #[abi(with = "bytes_as_string")]
    pub event_abi: String,
    #[abi(with = "address_only_hash")]
    pub staking: UInt256,
    #[abi(uint64)]
    pub event_initial_balance: u64,
    #[abi(cell)]
    pub event_code: Cell,
}

#[derive(Debug, Clone, PackAbi, UnpackAbi, KnownParamType)]
pub struct EvmTvmEventConfiguration {
    #[abi(uint32)]
    pub chain_id: u32,
    #[abi(with = "uint160_bytes")]
    pub event_emitter: [u8; 20],
    #[abi(uint16)]
    pub event_blocks_to_confirm: u16,
    #[abi(with = "address_only_hash")]
    pub proxy: UInt256,
    #[abi(uint32)]
    pub start_block_number: u32,
    #[abi(uint32)]
    pub end_block_number: u32,
}

#[derive(Debug, Clone, PackAbi, UnpackAbi, KnownParamType)]
pub struct TvmEvmEventConfiguration {
    #[abi(with = "address_only_hash")]
    pub event_emitter: UInt256,
    #[abi(with = "uint160_bytes")]
    pub proxy: [u8; 20],
    #[abi(uint32)]
    pub start_timestamp: u32,
    #[abi(uint32)]
    pub end_timestamp: u32,
}

#[derive(Debug, Clone, PackAbi, UnpackAbi, KnownParamType)]
pub struct SvmTvmEventConfiguration {
    #[abi(uint256)]
    pub program: UInt256,
    #[abi(with = "address_only_hash")]
    pub proxy: UInt256,
    #[abi(uint64)]
    pub start_timestamp: u64,
    #[abi(uint64)]
    pub end_timestamp: u64,
}

#[derive(Debug, Clone, PackAbi, UnpackAbi, KnownParamType)]
pub struct TvmSvmEventConfiguration {
    #[abi(uint256)]
    pub program: UInt256,
    #[abi(with = "address_only_hash")]
    pub event_emitter: UInt256,
    #[abi(uint8)]
    pub instruction: u8,
    #[abi(uint32)]
    pub start_timestamp: u32,
    #[abi(uint32)]
    pub end_timestamp: u32,
    #[abi(bool)]
    pub execute_needed: bool,
    #[abi(uint8)]
    pub execute_instruction: u8,
    #[abi(uint8)]
    pub execute_payload_instruction: u8,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, PackAbi, UnpackAbi, KnownParamType)]
pub enum EventType {
    EvmTvm = 0,
    TvmEvm = 1,
    SvmTvm = 2,
    TvmSvm = 3,
    TvmTvm = 4,
}

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EvmTvm => f.write_str("EVM->TVM"),
            Self::TvmEvm => f.write_str("TVM->EVM"),
            Self::SvmTvm => f.write_str("SVM->TVM"),
            Self::TvmSvm => f.write_str("TVM->SVM"),
            Self::TvmTvm => f.write_str("TVM->TVM"),
        }
    }
}

#[derive(Debug, Clone, PackAbiPlain, UnpackAbiPlain, KnownParamTypePlain)]
pub struct BridgeDetails {
    #[abi(cell)]
    pub connector_code: Cell,
    #[abi(uint64)]
    pub connector_deploy_value: u64,
    #[abi(uint64)]
    pub connector_counter: u64,
    #[abi(with = "address_only_hash")]
    pub staking: UInt256,
    #[abi(bool)]
    pub active: bool,
}

#[derive(Debug, Copy, Clone, PackAbiPlain, UnpackAbiPlain, KnownParamTypePlain)]
pub struct ConnectorDetails {
    #[abi(uint64)]
    pub id: u64,
    #[abi(with = "address_only_hash")]
    pub event_configuration: UInt256,
    #[abi(bool)]
    pub enabled: bool,
}

#[derive(Debug, Clone, PackAbiPlain, UnpackAbiPlain, KnownParamTypePlain)]
pub struct ConnectorDeployedEvent {
    #[abi(uint64)]
    pub id: u64,
    #[abi(with = "address_only_hash")]
    pub connector: UInt256,
    #[abi(with = "address_only_hash")]
    pub event_configuration: UInt256,
}

#[derive(Debug, Clone, PackAbiPlain, UnpackAbiPlain, KnownParamTypePlain)]
pub struct RelayRoundInitializedEvent {
    #[abi(uint32)]
    pub round_num: u32,
    #[abi(uint32)]
    pub round_start_time: u32,
    #[abi(uint32)]
    pub round_end_time: u32,
    #[abi(with = "address_only_hash")]
    pub round_addr: UInt256,
    #[abi(uint32)]
    pub relays_count: u32,
    #[abi(bool)]
    pub duplicate: bool,
}

#[derive(Debug, Clone, UnpackAbiPlain, KnownParamTypePlain)]
pub struct StakerAddresses {
    #[abi(with = "array_address_only_nonzero_hash")]
    pub items: Vec<UInt256>,
}

pub mod array_address_only_nonzero_hash {
    use super::*;
    use ton_abi::{ParamType, TokenValue};
    use ton_block::{MsgAddrStd, MsgAddress};

    pub fn unpack(value: &TokenValue) -> UnpackerResult<Vec<UInt256>> {
        match value {
            TokenValue::Array(_, values) => {
                let mut result = Vec::with_capacity(values.len());
                for value in values {
                    match value {
                        TokenValue::Address(MsgAddress::AddrStd(MsgAddrStd {
                            address, ..
                        })) => result.push(UInt256::from_be_bytes(&address.get_bytestring(0))),
                        TokenValue::Address(MsgAddress::AddrNone) => continue,
                        _ => return Err(UnpackerError::InvalidAbi),
                    }
                }
                Ok(result)
            }
            _ => Err(UnpackerError::InvalidAbi),
        }
    }

    pub fn param_type() -> ParamType {
        ParamType::Array(Box::new(ParamType::Address))
    }
}

#[derive(Debug, Clone, PackAbiPlain, UnpackAbiPlain, KnownParamTypePlain)]
pub struct RelayKeys {
    #[abi(array)]
    pub items: Vec<UInt256>,
}

#[derive(Debug, Clone, PackAbi, UnpackAbi, KnownParamType)]
pub struct UserDataDetails {
    #[abi]
    pub token_balance: u128,
    #[abi]
    pub relay_lock_until: u32,
    #[abi]
    pub current_version: u32,
    #[abi(array)]
    pub reward_rounds: Vec<UserDataRewardRound>,
    #[abi(name = "relay_eth_address", with = "uint160_bytes")]
    pub relay_evm_address: [u8; 20],
    #[abi(name = "eth_address_confirmed")]
    pub evm_address_confirmed: bool,
    #[abi(name = "relay_ton_pubkey")]
    pub relay_tvm_pubkey: UInt256,
    #[abi(name = "ton_pubkey_confirmed")]
    pub tvm_pubkey_confirmed: bool,
    #[abi]
    pub slashed: bool,
    #[abi(with = "address_only_hash")]
    pub root: UInt256,
    #[abi(with = "address_only_hash")]
    pub user: UInt256,
    #[abi(with = "address_only_hash")]
    pub dao_root: UInt256,
}

#[derive(Debug, Clone, PackAbi, UnpackAbi, KnownParamType)]
pub struct UserDataRewardRound {
    #[abi(uint128)]
    pub reward_balance: u128,
    #[abi(uint128)]
    pub reward_debt: u128,
}

#[cfg(not(feature = "disable-staking"))]
#[derive(Debug, Clone, PackAbi, UnpackAbi, KnownParamType)]
pub struct StakingDetails {
    #[abi(with = "address_only_hash")]
    pub dao_root: UInt256,
    #[abi(name = "bridge_event_config_eth_ton", with = "address_only_hash")]
    pub bridge_event_config_evm_tvm: UInt256,
    #[abi(name = "bridge_event_config_ton_eth", with = "address_only_hash")]
    pub bridge_event_config_tvm_evm: UInt256,
    #[abi(with = "address_only_hash")]
    pub token_root: UInt256,
    #[abi(with = "address_only_hash")]
    pub token_wallet: UInt256,
    #[abi(with = "address_only_hash")]
    pub admin: UInt256,
    #[abi(with = "address_only_hash")]
    pub rescuer: UInt256,
    #[abi(with = "address_only_hash")]
    pub rewarder: UInt256,
    #[abi(uint128)]
    pub token_balance: u128,
    #[abi(uint128)]
    pub reward_token_balance: u128,
    #[abi(uint32)]
    pub last_reward_time: u32,
    #[abi(array)]
    pub reward_rounds: Vec<RewardRound>,
    #[abi(bool)]
    pub emergency: bool,
}

#[derive(Debug, Clone, PackAbi, UnpackAbi, KnownParamType)]
pub struct RelayConfigDetails {
    #[abi(uint32)]
    pub relay_lock_time: u32,
    #[abi(uint32)]
    pub relay_round_time: u32,
    #[abi(uint32)]
    pub election_time: u32,
    #[abi(uint32)]
    pub time_before_election: u32,
    #[abi(uint32)]
    pub min_round_gap_time: u32,
    #[abi(uint16)]
    pub relays_count: u16,
    #[abi(uint16)]
    pub min_relay_count: u16,
    #[abi(uint128)]
    pub min_relay_deposit: u128,
    #[abi(name = "relay_initial_ton_deposit")]
    pub relay_initial_deposit: u128,
    #[abi(uint128)]
    pub relay_reward_per_second: u128,
    #[abi(uint128)]
    pub user_reward_per_second: u128,
}

#[derive(Debug, Clone, PackAbiPlain, UnpackAbiPlain, KnownParamTypePlain)]
pub struct RelayConfigUpdatedEvent {
    #[abi]
    pub config: RelayConfigDetails,
}

#[derive(Debug, Clone, PackAbi, UnpackAbi, KnownParamType)]
pub struct RelayRoundsDetails {
    #[abi(uint32)]
    pub current_relay_round: u32,
    #[abi(uint32)]
    pub current_relay_round_start_time: u32,
    #[abi(uint32)]
    pub current_relay_round_end_time: u32,
    #[abi(uint32)]
    pub current_election_start_time: u32,
    #[abi(bool)]
    pub current_election_ended: bool,
}

#[derive(Debug, Clone, PackAbi, UnpackAbi, KnownParamType)]
pub struct RewardRound {
    #[abi(uint256)]
    pub account_reward_per_share: UInt256,
    #[abi(uint128)]
    pub reward_tokens: u128,
    #[abi(uint128)]
    pub total_reward: u128,
    #[abi(uint32)]
    pub start_time: u32,
}

#[derive(Debug, Clone, PackAbiPlain, UnpackAbiPlain, KnownParamTypePlain)]
pub struct ElectionStartedEvent {
    #[abi(uint32)]
    pub round_num: u32,
    #[abi(uint32)]
    pub election_start_time: u32,
    #[abi(uint32)]
    pub election_end_time: u32,
    #[abi(with = "address_only_hash")]
    pub election_addr: UInt256,
}

#[derive(Debug, Clone, PackAbiPlain, UnpackAbiPlain, KnownParamTypePlain)]
pub struct ElectionEndedEvent {
    #[abi(uint32)]
    pub round_num: u32,
    #[abi(uint32)]
    pub relay_requests: u32,
    #[abi(bool)]
    pub min_relays_ok: bool,
}

#[derive(Debug, Clone, PackAbiPlain, UnpackAbiPlain, KnownParamTypePlain)]
pub struct TvmPubkeyConfirmedEvent {
    #[abi(name = "ton_pubkey")]
    pub tvm_pubkey: UInt256,
}

#[derive(Debug, Clone, PackAbiPlain, UnpackAbiPlain, KnownParamTypePlain)]
pub struct EvmAddressConfirmedEvent {
    #[abi(name = "eth_addr", with = "uint160_bytes")]
    pub evm_address: [u8; 20],
}

#[derive(Debug, Clone, PackAbiPlain, UnpackAbiPlain, KnownParamTypePlain)]
pub struct DepositProcessedEvent {
    #[abi(uint128)]
    pub tokens_deposited: u128,
    #[abi(uint128)]
    pub new_balance: u128,
}

#[derive(Debug, Clone, PackAbiPlain, UnpackAbiPlain, KnownParamTypePlain)]
pub struct RelayKeysUpdatedEvent {
    #[abi(name = "ton_pubkey")]
    pub tvm_pubkey: UInt256,
    #[abi(name = "eth_address", with = "uint160_bytes")]
    pub evm_address: [u8; 20],
}

#[derive(Debug, Clone, PackAbiPlain, UnpackAbiPlain, KnownParamTypePlain)]
pub struct RelayMembershipRequestedEvent {
    #[abi]
    pub round_num: u32,
    #[abi]
    pub tokens: u128,
    #[abi(name = "ton_pubkey")]
    pub tvm_pubkey: UInt256,
    #[abi(name = "eth_address", with = "uint160_bytes")]
    pub evm_address: [u8; 20],
    #[abi]
    pub lock_until: u32,
}

#[derive(Debug, Clone, UnpackAbi, KnownParamType)]
pub struct RelayRoundDetails {
    #[abi(with = "address_only_hash")]
    pub root: UInt256,
    #[abi]
    pub round_num: u32,
    #[abi(name = "ton_keys")]
    pub tvm_keys: Vec<UInt256>,
    #[abi(name = "eth_addrs", with = "array_uint160_bytes")]
    pub evm_addresses: Vec<[u8; 20]>,
    #[abi(with = "array_address_only_hash", name = "staker_addrs")]
    pub staker_addresses: Vec<UInt256>,
    #[abi]
    pub staked_tokens: Vec<u128>,
    #[abi]
    pub relays_installed: bool,
    #[abi]
    pub code_version: u32,
}
