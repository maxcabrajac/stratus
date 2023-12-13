use crate::eth::primitives::Address;
use crate::eth::primitives::Bytes;
use crate::eth::primitives::Execution;
use crate::eth::EthCall;
use crate::eth::EthDeployment;
use crate::eth::EthError;
use crate::eth::EthTransaction;

/// EVM operations.
pub trait Evm: Send + Sync + 'static {
    /// Execute a transaction that deploys a contract or call a function of a deployed contract.
    fn transact(&mut self, input: EvmInput) -> Result<Execution, EthError>;
}

pub struct EvmInput {
    pub caller: Address,
    pub contract: Option<Address>,
    pub data: Bytes,
}

impl From<EthDeployment> for EvmInput {
    fn from(value: EthDeployment) -> Self {
        Self {
            caller: value.caller,
            contract: None,
            data: value.data,
        }
    }
}

impl From<EthTransaction> for EvmInput {
    fn from(value: EthTransaction) -> Self {
        Self {
            caller: value.caller,
            contract: Some(value.contract),
            data: value.data,
        }
    }
}

impl From<EthCall> for EvmInput {
    fn from(value: EthCall) -> Self {
        Self {
            caller: Address::ZERO,
            contract: Some(value.contract),
            data: value.data,
        }
    }
}
