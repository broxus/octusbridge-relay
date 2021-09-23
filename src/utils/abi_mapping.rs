use anyhow::Result;
use ethabi::{ParamType as EthParamType, Token as EthTokenValue};
use num_bigint::{BigInt, BigUint};
use serde::{Deserialize, Serialize};
use ton_abi::{ParamType as TonParamType, TokenValue as TonTokenValue};

pub struct EthEventAbi {
    event: ethabi::Event,
    params: Vec<EthParamType>,
}

impl EthEventAbi {
    pub fn new(abi: &str) -> Result<Self> {
        let event = decode_eth_event_abi(abi)?;
        let params = event.inputs.iter().map(|item| item.kind.clone()).collect();
        Ok(Self { event, params })
    }

    pub fn get_eth_topic_hash(&self) -> [u8; 32] {
        self.event.signature().to_fixed_bytes()
    }

    pub fn decode_and_map(&self, data: &[u8]) -> Result<ton_types::Cell> {
        let tokens = ethabi::decode(&self.params, data)?;
        map_eth_tokens_to_ton_cell(tokens, &self.params)
    }
}

pub fn decode_ton_event_abi(abi: &str) -> Result<Vec<ton_abi::Param>> {
    let params = serde_json::from_str::<Vec<ton_abi::Param>>(abi)?;
    Ok(params)
}

pub fn decode_eth_event_abi(abi: &str) -> Result<ethabi::Event> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    pub enum Operation {
        Event(ethabi::Event),
    }

    serde_json::from_str::<Operation>(abi)
        .map(|item| match item {
            Operation::Event(event) => event,
        })
        .map_err(anyhow::Error::from)
}

pub fn map_eth_abi_to_ton<'a, I>(abi: I, can_update_ctx: bool) -> Result<Vec<TonParamType>>
where
    I: Iterator<Item = &'a EthParamType>,
{
    abi.filter_map(|param| map_eth_abi_param_to_ton(param, can_update_ctx).transpose())
        .collect()
}

fn map_eth_abi_param_to_ton(
    param: &EthParamType,
    can_update_ctx: bool,
) -> Result<Option<TonParamType>> {
    Ok(Some(match param {
        EthParamType::Address => TonParamType::Bytes,
        EthParamType::Bytes => TonParamType::Bytes,
        EthParamType::Int(size) => TonParamType::Int(*size),
        EthParamType::Uint(size) => TonParamType::Uint(*size),
        EthParamType::Bool => TonParamType::Bool,
        EthParamType::String => TonParamType::String,
        EthParamType::Array(param) => match map_eth_abi_param_to_ton(param.as_ref(), false)? {
            Some(param) => TonParamType::Array(Box::new(param)),
            None => return Ok(None),
        },
        &EthParamType::FixedBytes(1) if can_update_ctx => return Ok(None),
        EthParamType::FixedBytes(size) => TonParamType::FixedBytes(*size),
        EthParamType::FixedArray(param, size) => {
            match map_eth_abi_param_to_ton(param.as_ref(), false)? {
                Some(param) => TonParamType::FixedArray(Box::new(param), *size),
                None => return Ok(None),
            }
        }
        EthParamType::Tuple(params) => TonParamType::Tuple(
            params
                .iter()
                .filter_map(|item| {
                    map_eth_abi_param_to_ton(item, can_update_ctx)
                        .transpose()
                        .map(|kind| {
                            kind.map(|kind| ton_abi::Param {
                                name: String::new(),
                                kind,
                            })
                        })
                })
                .collect::<Result<Vec<ton_abi::Param>>>()?,
        ),
    }))
}

/// struct TONEvent {
///     uint64 eventTransactionLt;
///     uint32 eventTimestamp;
///     bytes eventData;
///     int8 configurationWid;
///     uint256 configurationAddress;
///     int8 eventContractWid;
///     uint256 eventContractAddress;
///     address proxy;
///     uint32 round;
/// }
pub fn make_mapped_ton_event(
    event_transaction_lt: u64,
    event_timestamp: u32,
    event_data: Vec<u8>,
    configuration: ton_types::UInt256,
    event_account: ton_types::UInt256,
    proxy: [u8; 20],
    round: u32,
) -> Vec<u8> {
    ethabi::encode(&[ethabi::Token::Tuple(vec![
        ethabi::Token::Uint(event_transaction_lt.into()),
        ethabi::Token::Uint(event_timestamp.into()),
        ethabi::Token::Bytes(event_data),
        ethabi::Token::Int(0i8.into()),
        ethabi::Token::Uint(configuration.as_slice().into()),
        ethabi::Token::Int(0i8.into()),
        ethabi::Token::Uint(event_account.as_slice().into()),
        ethabi::Token::Address(proxy.into()),
        ethabi::Token::Uint(round.into()),
    ])])
}

/// Maps `Vec<TonTokenValue>` to bytes, which could be signed
pub fn map_ton_tokens_to_eth_bytes(tokens: Vec<ton_abi::Token>) -> Result<Vec<u8>> {
    let tokens = tokens
        .into_iter()
        .map(|token| token.value)
        .map(map_ton_token_to_eth)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ethabi::encode(&tokens))
}

pub fn map_eth_tokens_to_ton_cell(
    tokens: Vec<EthTokenValue>,
    abi: &[EthParamType],
) -> Result<ton_types::Cell> {
    let mut ctx = EthToTonMappingContext::default();

    let tokens = tokens
        .into_iter()
        .zip(abi.iter())
        .filter_map(|(token, param)| map_eth_token_to_ton(token, param, true, &mut ctx).transpose())
        .collect::<Result<Vec<TonTokenValue>>>()?;

    let cells = Vec::with_capacity(tokens.len());
    ton_abi::TokenValue::pack_token_values_into_chain(&tokens, cells, 2)
        .and_then(|builder| builder.into_cell())
}

#[derive(Default, Debug, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct EthToTonMappingContext {
    /// Starts new cell for each tuple.
    ///
    /// See `MAPPING_FLAG_TUPLES_TO_NEW_CELL`
    pub tuples_to_new_cell: bool,

    /// Interprets bytes ad cell
    ///
    /// See `MAPPING_FLAG_BYTES_AS_CELL`
    pub bytes_as_cell: bool,
}

impl EthToTonMappingContext {
    pub fn update(&mut self, flags: u8) {
        self.tuples_to_new_cell = flags & MAPPING_FLAG_TUPLES_TO_NEW_CELL != 0;
        self.bytes_as_cell = flags & MAPPING_FLAG_BYTES_AS_CELL != 0;
    }
}

pub const MAPPING_FLAG_TUPLES_TO_NEW_CELL: u8 = 0b00000001;
pub const MAPPING_FLAG_BYTES_AS_CELL: u8 = 0b00000010;

impl From<u8> for EthToTonMappingContext {
    fn from(flags: u8) -> Self {
        let mut ctx = Self::default();
        ctx.update(flags);
        ctx
    }
}

pub fn map_eth_token_to_ton(
    token: EthTokenValue,
    param: &EthParamType,
    can_update_ctx: bool,
    ctx: &mut EthToTonMappingContext,
) -> Result<Option<TonTokenValue>> {
    Ok(Some(match (token, param) {
        (EthTokenValue::FixedBytes(x), EthParamType::FixedBytes(1)) if can_update_ctx => {
            let flags = *x.get(0).ok_or(AbiMappingError::InvalidMappingFlags)?;
            ctx.update(flags);
            return Ok(None);
        }
        (EthTokenValue::FixedBytes(x), _) => TonTokenValue::FixedBytes(x.to_vec()),
        (EthTokenValue::Bytes(x), _) => {
            if ctx.bytes_as_cell {
                let data = base64::decode(&x)?;
                TonTokenValue::Cell(ton_types::deserialize_tree_of_cells(
                    &mut std::io::Cursor::new(data),
                )?)
            } else {
                TonTokenValue::Bytes(x.to_vec())
            }
        }
        (EthTokenValue::Uint(x), &EthParamType::Uint(size)) => {
            let mut bytes = [0u8; 256 / 8];
            x.to_big_endian(&mut bytes);
            let number = BigUint::from_bytes_be(&bytes);
            TonTokenValue::Uint(ton_abi::Uint { number, size })
        }
        (EthTokenValue::Int(x), &EthParamType::Int(size)) => {
            let mut bytes = [0u8; 256 / 8];
            x.to_big_endian(&mut bytes);
            let number = BigInt::from_signed_bytes_be(&bytes);
            TonTokenValue::Int(ton_abi::Int { number, size })
        }
        (EthTokenValue::Address(ad), _) => TonTokenValue::Bytes(ad.0.to_vec()),
        (EthTokenValue::String(a), _) => TonTokenValue::String(a),
        (EthTokenValue::Bool(a), _) => TonTokenValue::Bool(a),
        (EthTokenValue::FixedArray(a), EthParamType::FixedArray(abi, _)) => {
            let param_type = match *abi.clone() {
                EthParamType::Array(arr) => {
                    let mut mapped = map_eth_abi_to_ton(std::iter::once(arr.as_ref()), false)?;
                    anyhow::ensure!(!mapped.is_empty(), "No types");
                    mapped.remove(0)
                }
                _ => anyhow::bail!("Bad abi"),
            };
            TonTokenValue::FixedArray(
                param_type,
                a.into_iter()
                    .filter_map(|value| map_eth_token_to_ton(value, abi, false, ctx).transpose())
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
        (EthTokenValue::Array(a), EthParamType::Array(abi)) => {
            let param_type = match *abi.clone() {
                EthParamType::Array(arr) => {
                    let mut mapped = map_eth_abi_to_ton(std::iter::once(arr.as_ref()), false)?;
                    anyhow::ensure!(!mapped.is_empty(), "No types");
                    mapped.remove(0)
                }
                _ => anyhow::bail!("Bad abi"),
            };
            TonTokenValue::Array(
                param_type,
                a.into_iter()
                    .filter_map(|value| map_eth_token_to_ton(value, abi, false, ctx).transpose())
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
        (EthTokenValue::Tuple(a), EthParamType::Tuple(abi)) => {
            // NOTE: save flag before processing tokens to prevent updating
            let to_new_cell = ctx.tuples_to_new_cell;

            let tokens = a
                .into_iter()
                .zip(abi.iter())
                .filter_map(|(value, abi)| {
                    map_eth_token_to_ton(value, abi, can_update_ctx, ctx).transpose()
                })
                .collect::<Result<Vec<_>, _>>()?;

            if to_new_cell {
                TonTokenValue::Cell(
                    ton_abi::TokenValue::pack_token_values_into_chain(
                        &tokens,
                        Default::default(),
                        2,
                    )?
                    .into(),
                )
            } else {
                TonTokenValue::Tuple(
                    tokens
                        .into_iter()
                        .map(|x| ton_abi::Token::new("", x))
                        .collect(),
                )
            }
        }
        ty => return Err(AbiMappingError::UnsupportedEthType(ty.0).into()),
    }))
}

fn map_ton_token_to_eth(token: TonTokenValue) -> Result<EthTokenValue, AbiMappingError> {
    Ok(match token {
        TonTokenValue::FixedBytes(bytes) => EthTokenValue::FixedBytes(bytes),
        TonTokenValue::Bytes(bytes) => EthTokenValue::Bytes(bytes),
        TonTokenValue::Uint(a) => {
            let bytes = a.number.to_bytes_le();
            EthTokenValue::Uint(ethabi::Uint::from_little_endian(&bytes))
        }
        TonTokenValue::Int(a) => {
            let mut bytes = a.number.to_signed_bytes_le();
            let sign = bytes
                .last()
                .map(|first| (first >> 7) * 255)
                .unwrap_or_default();
            bytes.resize(32, sign);
            //fixme check it
            EthTokenValue::Int(ethabi::Int::from_little_endian(&bytes))
        }
        TonTokenValue::Bool(a) => EthTokenValue::Bool(a),
        TonTokenValue::FixedArray(_, tokens) => EthTokenValue::FixedArray(
            tokens
                .into_iter()
                .map(map_ton_token_to_eth)
                .collect::<Result<_, _>>()?,
        ),
        TonTokenValue::Array(_, tokens) => EthTokenValue::Array(
            tokens
                .into_iter()
                .map(map_ton_token_to_eth)
                .collect::<Result<_, _>>()?,
        ),
        TonTokenValue::Tuple(tokens) => EthTokenValue::Tuple(
            tokens
                .into_iter()
                .map(|ton| map_ton_token_to_eth(ton.value))
                .collect::<Result<_, _>>()?,
        ),
        TonTokenValue::String(a) => EthTokenValue::String(a),
        any => return Err(AbiMappingError::UnsupportedTonType(any)),
    })
}

#[derive(thiserror::Error, Debug)]
enum AbiMappingError {
    #[error("Unsupported type: {:?}", .0)]
    UnsupportedTonType(ton_abi::TokenValue),
    #[error("Unsupported type: {:?}", .0)]
    UnsupportedEthType(ethabi::Token),
    #[error("Invalid mapping flags")]
    InvalidMappingFlags,
}

#[cfg(test)]
mod test {
    use super::*;
    use nekoton_abi::{BuildTokenValue, TokenValueExt};
    use pretty_assertions::assert_eq;
    use ton_types::UInt256;

    #[test]
    fn test_eth_ton() {
        let types = [
            EthParamType::Address,
            EthParamType::String,
            EthParamType::Int(128),
            EthParamType::Tuple(vec![EthParamType::Bool, EthParamType::Bytes]),
        ];
        let got = super::map_eth_abi_to_ton(types.iter(), true).unwrap();
        let expected = vec![
            TonParamType::Bytes,
            TonParamType::String,
            TonParamType::Int(128),
            TonParamType::Tuple(vec![
                ton_abi::Param {
                    name: "".to_string(),
                    kind: TonParamType::Bool,
                },
                ton_abi::Param {
                    name: "".to_string(),
                    kind: TonParamType::Bytes,
                },
            ]),
        ];
        assert_eq!(got, expected);
    }

    fn test_int(size: u16, number: i128) {
        let ton_token = TonTokenValue::Int(ton_abi::Int::new(number, size as usize));
        let eth_token = map_ton_token_to_eth(ton_token).unwrap();
        match eth_token {
            EthTokenValue::Int(a) => {
                assert_eq!(a.to_string(), number.to_string());
            }
            _ => unreachable!(),
        }
    }

    fn test_uint(size: u16, number: u128) {
        let ton_token = TonTokenValue::Uint(ton_abi::Uint::new(number, size as usize));
        let eth_token = map_ton_token_to_eth(ton_token).unwrap();
        match eth_token {
            EthTokenValue::Uint(a) => {
                assert_eq!(a.as_u128(), number);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn test_ton_eth_uint() {
        let mut i = 1;
        loop {
            test_uint(128, i as u128);
            if i >= u128::MAX / 2 {
                break;
            }
            i *= 2;
        }
    }

    #[test]
    fn test_ton_eth_int() {
        let mut i = 1;
        loop {
            test_int(128, i);
            if i >= i128::MAX / 2 {
                break;
            }
            i *= 2;
        }
    }

    #[test]
    fn test_conversion_tuple() {
        use ethabi::Uint as EUnt;
        use ton_abi::Uint as TUInt;

        let number = 1234567;
        let ton_token_uint = ton_abi::Token {
            name: "".to_string(),
            value: TonTokenValue::Uint(TUInt::new(number, 256)),
        };
        let ton_token_bytes = ton_abi::Token {
            name: "".to_string(),
            value: TonTokenValue::Bytes("hello from rust".to_string().into()),
        };
        let eth_token_uint = ethabi::Token::Uint(EUnt::from(number));
        let eth_token_bytes = ethabi::Token::Bytes("hello from rust".to_string().into());
        let eth = EthTokenValue::Tuple(vec![eth_token_uint, eth_token_bytes]);
        let ton_expected = TonTokenValue::Tuple(vec![ton_token_uint, ton_token_bytes]);
        assert_eq!(
            map_eth_token_to_ton(
                eth,
                &ethabi::ParamType::Tuple(vec![
                    ethabi::ParamType::Uint(256),
                    ethabi::ParamType::Bytes
                ]),
                true,
                &mut Default::default()
            )
            .unwrap()
            .unwrap(),
            ton_expected
        );
    }

    const ABI: &str = r#"{
    "anonymous": false,
    "inputs": [
        {
            "indexed": false,
            "name": "state",
            "type": "uint256"
        },
        {
            "indexed": false,
            "name": "author",
            "type": "address"
        }
    ],
    "outputs": [],
    "name": "StateChange"
}"#;

    const ABI2: &str = r#"{
    "inputs": [
        {
            "name": "a",
            "type": "address",
            "indexed": true
        },
        {
            "components": [
                {
                    "internalType": "address",
                    "name": "to",
                    "type": "address"
                },
                {
                    "internalType": "uint256",
                    "name": "value",
                    "type": "uint256"
                },
                {
                    "internalType": "bytes",
                    "name": "data",
                    "type": "bytes"
                }
            ],
            "indexed": false,
            "internalType": "struct Action[]",
            "name": "b",
            "type": "tuple[]"
        }
    ],
    "name": "E",
    "outputs": [],
    "anonymous": false
}"#;

    const ABI3: &str = r#"{
    "name": "TokenLock",
    "anonymous": false,
    "inputs": [
        {
            "name": "amount",
            "type": "uint128"
        },
        {
            "name": "wid",
            "type": "int8"
        },
        {
            "name": "addr",
            "type": "uint256"
        },
        {
            "name": "pubkey",
            "type": "uint256"
        }
    ],
    "outputs": []
}"#;

    #[test]
    fn test_bad_abi() {
        assert!(EthEventAbi::new("lol").is_err());
    }

    #[test]
    fn test_abi() {
        let expected = web3::signing::keccak256(b"StateChange(uint256,address)");
        let abi = EthEventAbi::new(ABI).unwrap();
        assert_eq!(expected, abi.get_eth_topic_hash());
    }

    #[test]
    fn test_abi2() {
        EthEventAbi::new(ABI2).unwrap();
    }

    #[test]
    fn test_abi3() {
        EthEventAbi::new(ABI3).unwrap();
    }

    #[test]
    fn test_abi4() {
        // Withdrawal event
        decode_ton_event_abi(
            r#"[
                {"name":"wid","type":"int8"},
                {"name":"addr","type":"uint256"},
                {"name":"tokens","type":"uint128"},
                {"name":"eth_addr","type":"uint160"},
                {"name":"chainId","type":"uint32"}
            ]"#,
        )
        .unwrap();

        let bytes = map_ton_tokens_to_eth_bytes(vec![
            0i8.token_value().named("wid"),
            UInt256::default().token_value().named("addr"),
            123u128.token_value().named("tokens"),
            nekoton_abi::uint160_bytes::pack([1u8; 20]).named("eth_addr"),
            5u32.token_value().named("chainId"),
        ])
        .unwrap();
        println!("Withdrawal event: {}", hex::encode(&bytes));

        // DAO event
        decode_ton_event_abi(
            r#"[
                {"name":"gasBackWid","type":"int8"},
				{"name":"gasBackAddress","type":"uint256"},
				{"name":"chainId","type":"uint32"},
				{"components":[{"name":"value","type":"uint256"},{"name":"target","type":"uint160"},{"name":"signature","type":"string"},{"name":"callData","type":"bytes"}],"name":"actions","type":"tuple[]"}

            ]"#,
        )
        .unwrap();

        let bytes = map_ton_tokens_to_eth_bytes(vec![
            0i8.token_value().named("gasBackWid"),
            UInt256::default().token_value().named("gasBackAddress"),
            5u32.token_value().named("chainId"),
            ton_abi::Token::new(
                "actions",
                ton_abi::TokenValue::Array(
                    ton_abi::ParamType::Tuple(vec![
                        ton_abi::Param::new("value", ton_abi::ParamType::Uint(256)),
                        ton_abi::Param::new("target", ton_abi::ParamType::Uint(160)),
                        ton_abi::Param::new("signature", ton_abi::ParamType::String),
                        ton_abi::Param::new("callData", ton_abi::ParamType::Bytes),
                    ]),
                    vec![ton_abi::TokenValue::Tuple(vec![
                        UInt256::default().token_value().named("value"),
                        nekoton_abi::uint160_bytes::pack([1u8; 20]).named("target"),
                        ton_abi::TokenValue::String("asd".to_string()).named("signature"),
                        vec![1u8, 2, 3].token_value().named("callData"),
                    ])],
                ),
            ),
        ])
        .unwrap();
        println!("DaoRoot event: {}", hex::encode(&bytes));

        // Staking event
        decode_ton_event_abi(
            r#"[
				{"name":"round_num","type":"uint32"},
				{"name":"eth_keys","type":"uint160[]"},
				{"name":"round_end","type":"uint32"}
			]"#,
        )
        .unwrap();

        let bytes = map_ton_tokens_to_eth_bytes(vec![
            123u32.token_value().named("round_num"),
            ton_abi::Token::new(
                "eth_keys",
                ton_abi::TokenValue::Array(
                    ton_abi::ParamType::Uint(160),
                    vec![
                        nekoton_abi::uint160_bytes::pack([1u8; 20]),
                        nekoton_abi::uint160_bytes::pack([2u8; 20]),
                        nekoton_abi::uint160_bytes::pack([3u8; 20]),
                    ],
                ),
            ),
            9999u32.token_value().named("round_num"),
        ])
        .unwrap();
        println!("Staking event: {}", hex::encode(&bytes));
    }

    #[test]
    fn test_mapped_event() {
        let data = make_mapped_ton_event(
            123,
            321,
            map_ton_tokens_to_eth_bytes(vec![
                0i8.token_value().named("gasBackWid"),
                UInt256::default().token_value().named("gasBackAddress"),
                5u32.token_value().named("chainId"),
                ton_abi::Token::new(
                    "actions",
                    ton_abi::TokenValue::Array(
                        ton_abi::ParamType::Tuple(vec![
                            ton_abi::Param::new("value", ton_abi::ParamType::Uint(256)),
                            ton_abi::Param::new("target", ton_abi::ParamType::Uint(160)),
                            ton_abi::Param::new("signature", ton_abi::ParamType::String),
                            ton_abi::Param::new("callData", ton_abi::ParamType::Bytes),
                        ]),
                        vec![ton_abi::TokenValue::Tuple(vec![
                            UInt256::default().token_value().named("value"),
                            nekoton_abi::uint160_bytes::pack([1u8; 20]).named("target"),
                            ton_abi::TokenValue::String("asd".to_string()).named("signature"),
                            vec![1u8, 2, 3].token_value().named("callData"),
                        ])],
                    ),
                ),
            ])
            .unwrap(),
            UInt256::default(),
            UInt256::default(),
            [1u8; 20],
            123,
        );
        println!("{}", hex::encode(data));
    }

    #[test]
    fn test_abi_mapping_flags() {
        let tokens = vec![
            ethabi::Token::Bytes(vec![1, 2, 3]),
            ethabi::Token::FixedBytes(vec![0x03]),
            ethabi::Token::Tuple(vec![ethabi::Token::Bool(true)]),
        ];

        let cell = map_eth_tokens_to_ton_cell(
            tokens,
            &[
                ethabi::ParamType::Bytes,
                ethabi::ParamType::FixedBytes(1),
                ethabi::ParamType::Tuple(vec![ethabi::ParamType::Bool]),
            ],
        )
        .unwrap();

        println!("{:#.1024}", cell);
    }
}