use ethereum_types::H256;
use ethers_core::types::Block as EthersBlock;
use ethers_core::types::Transaction as EthersTransaction;
use itertools::Itertools;
use serde_json::Value as JsonValue;

use crate::eth::primitives::BlockHeader;
use crate::eth::primitives::BlockNumber;
use crate::eth::primitives::TransactionMined;

#[derive(Debug, Clone)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<TransactionMined>,
}

impl Block {
    /// Creates a new block with the given number.
    pub fn new(number: BlockNumber) -> Self {
        Self {
            header: BlockHeader::new(number),
            transactions: vec![],
        }
    }

    /// Creates a new block with the given number and transactions capacity.
    pub fn new_with_capacity(number: BlockNumber, capacity: usize) -> Self {
        Self {
            header: BlockHeader::new(number),
            transactions: Vec::with_capacity(capacity),
        }
    }

    /// Serializes itself with full transactions included.
    pub fn to_json_with_full_transactions(&self) -> JsonValue {
        let json_struct: EthersBlock<EthersTransaction> = self.into();
        serde_json::to_value(json_struct).unwrap()
    }

    /// Serializes itself with only transactions hashes included.
    pub fn to_json_with_transactions_hashes(&self) -> JsonValue {
        let json_struct: EthersBlock<H256> = self.into();
        serde_json::to_value(json_struct).unwrap()
    }
}

// -----------------------------------------------------------------------------
// Serialization / Deserialization
// -----------------------------------------------------------------------------
impl serde::Serialize for Block {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        Into::<EthersBlock<EthersTransaction>>::into(self).serialize(serializer)
    }
}

// -----------------------------------------------------------------------------
// Conversions: Self -> Other
// -----------------------------------------------------------------------------
impl From<&Block> for EthersBlock<EthersTransaction> {
    fn from(block: &Block) -> Self {
        let ethers_block = EthersBlock::<EthersTransaction>::from(block.header.clone());
        let ethers_block_transactions: Vec<EthersTransaction> = block.transactions.clone().into_iter().map_into().collect();
        Self {
            transactions: ethers_block_transactions,
            ..ethers_block
        }
    }
}

impl From<&Block> for EthersBlock<H256> {
    fn from(block: &Block) -> Self {
        let ethers_block = EthersBlock::<H256>::from(block.header.clone());
        let ethers_block_transactions: Vec<H256> = block.transactions.clone().into_iter().map(|x| x.input.hash).map_into().collect();
        Self {
            transactions: ethers_block_transactions,
            ..ethers_block
        }
    }
}
