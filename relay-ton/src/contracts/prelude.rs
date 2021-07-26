use nekoton_parser::abi::{IntoUnpacker, UnpackToken};
pub use ton_abi::{Token, TokenValue};

pub use super::contract::*;
use super::errors::*;
use crate::models::*;
use crate::prelude::*;

pub trait IgnoreOutput: Sized {
    fn ignore_output(self) -> Result<(), ContractError> {
        Ok(())
    }
}

impl IgnoreOutput for ContractOutput {}

pub trait ReadMethodId {
    type Error;

    fn read_method_id(&self) -> Result<u32, Self::Error>;
}

impl ReadMethodId for SliceData {
    type Error = failure::Error;

    fn read_method_id(&self) -> Result<u32, Self::Error> {
        let mut value: u32 = 0;
        for i in 0..4 {
            value |= (self.get_byte(8 * i)? as u32) << (8 * (3 - i));
        }
        Ok(value)
    }
}

macro_rules! impl_contract_output_tuple {
    ($($type:ident),+) => {
        impl<$($type),*> TryFrom<ContractOutput> for ($($type),*)
        where
            $(TokenValue: UnpackToken<$type>),*
        {
            type Error = ContractError;

            fn try_from(value: ContractOutput) -> Result<Self, Self::Error> {
                let mut tokens = value.tokens.into_iter();
                Ok((
                    $(UnpackToken::<$type>::unpack(tokens.next())?),*
                ))
            }
        }
    };
}

impl_contract_output_tuple!(T1, T2);
impl_contract_output_tuple!(T1, T2, T3);
impl_contract_output_tuple!(T1, T2, T3, T4);

#[macro_export]
macro_rules! define_event{
    ($event_name:ident, $event_kind:ident, { $($name:ident $({ $($data:tt)* })?),+$(,)? }) => {
        #[derive(Debug, Clone)]
        pub enum $event_name {
            $($name $({ $($data)* })?),+,
        }

        #[derive(Debug, Clone, Copy, Ord, PartialOrd, Eq, PartialEq, Hash)]
        pub enum $event_kind {
            $($name),+,
        }

        impl $event_kind {
            pub fn name(&self) -> &'static str {
                match self {
                    $(<$event_kind>::$name => stringify!($name)),*,
                }
            }
        }

        impl std::convert::TryFrom<&str> for $event_kind {
            type Error = ContractError;

            fn try_from(s: &str) -> ContractResult<Self> {
                match s {
                    $(stringify!($name) => Ok(Self::$name)),*,
                    _ => Err(ContractError::InvalidAbi)
                }
            }
        }
    };
}
