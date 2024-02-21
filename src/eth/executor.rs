//! EthExecutor: Ethereum Transaction Coordinator
//!
//! This module provides the `EthExecutor` struct, which acts as a coordinator for executing Ethereum transactions.
//! It encapsulates the logic for transaction execution, state mutation, and event notification.
//! `EthExecutor` is designed to work with the `Evm` trait implementations to execute transactions and calls,
//! while also interfacing with a miner component to handle block mining and a storage component to persist state changes.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;

use anyhow::anyhow;
use ethereum_types::H256;
use ethers_core::types::Block as EthersBlock;
use nonempty::NonEmpty;
use tokio::runtime::Handle;
use tokio::sync::broadcast;
use tokio::sync::oneshot;
use tokio::sync::Mutex;

use crate::eth::evm::Evm;
use crate::eth::evm::EvmInput;
use crate::eth::primitives::Block;
use crate::eth::primitives::CallInput;
use crate::eth::primitives::Execution;
use crate::eth::primitives::ExternalBlock;
use crate::eth::primitives::ExternalReceipt;
use crate::eth::primitives::ExternalTransaction;
use crate::eth::primitives::ExternalTransactionExecution;
use crate::eth::primitives::Hash;
use crate::eth::primitives::LogMined;
use crate::eth::primitives::StoragePointInTime;
use crate::eth::primitives::TransactionInput;
use crate::eth::storage::EthStorageError;
use crate::eth::storage::StratusStorage;
use crate::eth::BlockMiner;

/// Number of events in the backlog.
const NOTIFIER_CAPACITY: usize = u16::MAX as usize;

type EvmTask = (EvmInput, oneshot::Sender<anyhow::Result<Execution>>);

/// The EthExecutor struct is responsible for orchestrating the execution of Ethereum transactions.
/// It holds references to the EVM, block miner, and storage, managing the overall process of
/// transaction execution, block production, and state management.
pub struct EthExecutor {
    // Channel to send transactions to background EVMs.
    evm_tx: crossbeam_channel::Sender<EvmTask>,

    // Mutex-wrapped miner for creating new blockchain blocks.
    miner: Mutex<BlockMiner>,

    // Shared storage backend for persisting blockchain state.
    storage: Arc<StratusStorage>,

    // Broadcast channels for notifying subscribers about new blocks and logs.
    block_notifier: broadcast::Sender<Block>,
    log_notifier: broadcast::Sender<LogMined>,
}

impl EthExecutor {
    /// Creates a new executor.
    pub fn new(evms: NonEmpty<Box<dyn Evm>>, eth_storage: Arc<StratusStorage>) -> Self {
        let evm_tx = spawn_background_evms(evms);

        Self {
            evm_tx,
            miner: Mutex::new(BlockMiner::new(Arc::clone(&eth_storage))),
            storage: eth_storage,
            block_notifier: broadcast::channel(NOTIFIER_CAPACITY).0,
            log_notifier: broadcast::channel(NOTIFIER_CAPACITY).0,
        }
    }

    /// Imports an external block using the offline flow.
    pub async fn import_offline(&self, block: ExternalBlock, receipts: &HashMap<Hash, ExternalReceipt>) -> anyhow::Result<()> {
        tracing::info!(number = %block.number(), "importing offline block");

        // re-execute transactions
        let mut executions: Vec<ExternalTransactionExecution> = Vec::with_capacity(block.transactions.len());
        for tx in block.transactions.clone() {
            // find receipt
            let Some(receipt) = receipts.get(&tx.hash()).cloned() else {
                tracing::error!(hash = %tx.hash, "receipt is missing");
                return Err(anyhow!("receipt missing for hash {}", tx.hash));
            };

            // re-execute transaction
            let evm_input = EvmInput::from_external_transaction(&block, tx.clone(), &receipt);
            let execution = self.execute_in_evm(evm_input).await;

            // handle execution result
            match execution {
                Ok(execution) => {
                    // ensure it matches receipt before saving
                    if let Err(e) = execution.compare_with_receipt(&receipt) {
                        let json_tx = serde_json::to_string(&tx).unwrap();
                        let json_receipt = serde_json::to_string(&receipt).unwrap();
                        let json_execution_logs = serde_json::to_string(&execution.logs).unwrap();
                        tracing::error!(%json_tx, %json_receipt, %json_execution_logs, "mismatch reexecuting transaction");
                        return Err(e);
                    };

                    // temporarily save state to next transactions from the same block
                    self.storage.save_account_changes(block.number(), execution.clone()).await?;
                    executions.push((tx, receipt, execution));
                }
                Err(e) => {
                    let json_tx = serde_json::to_string(&tx).unwrap();
                    let json_receipt = serde_json::to_string(&receipt).unwrap();
                    tracing::error!(reason = ?e, %json_tx, %json_receipt, "unexpected error reexecuting transaction");
                    return Err(e);
                }
            }
        }

        let block = Block::from_external(block, executions)?;
        self.storage.increment_block_number().await?;
        if let Err(e) = self.storage.commit(block.clone()).await {
            let json_block = serde_json::to_string(&block).unwrap();
            tracing::error!(reason = ?e, %json_block);
            return Err(e.into());
        };

        Ok(())
    }

    pub async fn import(&self, external_block: ExternalBlock, external_receipts: HashMap<H256, ExternalReceipt>) -> anyhow::Result<()> {
        for external_transaction in <EthersBlock<ExternalTransaction>>::from(external_block.clone()).transactions {
            // Find the receipt for the current transaction.
            let external_receipt = external_receipts
                .get(&external_transaction.hash)
                .ok_or(anyhow!("receipt not found for transaction {}", external_transaction.hash))?;

            // TODO: this conversion should probably not be happening and instead the external transaction can be used directly
            let transaction_input: TransactionInput = match external_transaction.to_owned().try_into() {
                Ok(transaction_input) => transaction_input,
                Err(e) => return Err(anyhow!("failed to convert external transaction into TransactionInput: {:?}", e)),
            };

            let evm_input = EvmInput::from_eth_transaction(transaction_input.clone());
            let execution = self.execute_in_evm(evm_input).await?;

            execution.compare_with_receipt(external_receipt)?;

            let block = self.miner.lock().await.mine_with_one_transaction(transaction_input, execution).await?;

            self.storage.commit(block).await?;
        }

        //TODO compare slots/changes
        //TODO compare nonce
        //TODO compare balance
        //XXX panic in case of bad comparisson

        Ok(())
    }

    /// Executes Ethereum transactions and facilitates block creation.
    ///
    /// This function is a key part of the transaction processing pipeline. It begins by validating
    /// incoming transactions and then proceeds to execute them. Unlike conventional blockchain systems,
    /// the block creation here is not dictated by timed intervals but is instead triggered by transaction
    /// processing itself. This method encapsulates the execution, block mining, and state mutation,
    /// concluding with broadcasting necessary notifications for the newly created block and associated transaction logs.
    ///
    /// TODO: too much cloning that can be optimized here.
    pub async fn transact(&self, transaction: TransactionInput) -> anyhow::Result<Execution> {
        tracing::info!(
            hash = %transaction.hash,
            nonce = %transaction.nonce,
            from = ?transaction.from,
            signer = %transaction.signer,
            to = ?transaction.to,
            data_len = %transaction.input.len(),
            data = %transaction.input,
            "executing real transaction"
        );

        // validate
        if transaction.signer.is_zero() {
            tracing::warn!("rejecting transaction from zero address");
            return Err(anyhow!("Transaction sent from zero address is not allowed."));
        }

        //creates a block and performs the necessary notifications
        self.mine_and_execute_transaction(transaction).await
    }

    #[cfg(feature = "evm-mine")]
    pub async fn mine_empty_block(&self) -> anyhow::Result<()> {
        let mut miner_lock = self.miner.lock().await;
        let block = miner_lock.mine_with_no_transactions().await?;
        self.storage.commit(block.clone()).await?;

        if let Err(e) = self.block_notifier.send(block.clone()) {
            tracing::error!(reason = ?e, "failed to send block notification");
        };

        Ok(())
    }

    async fn mine_and_execute_transaction(&self, transaction: TransactionInput) -> anyhow::Result<Execution> {
        // execute transaction until no more conflicts
        // TODO: must have a stop condition like timeout or max number of retries
        let (execution, block) = loop {
            // execute and check conflicts before mining block
            let evm_input = EvmInput::from_eth_transaction(transaction.clone());
            let execution = self.execute_in_evm(evm_input).await?;
            if let Some(conflicts) = self.storage.check_conflicts(&execution).await? {
                tracing::warn!(?conflicts, "storage conflict detected before mining block");
                continue;
            }

            // mine and commit block
            let mut miner_lock = self.miner.lock().await;
            let block = miner_lock.mine_with_one_transaction(transaction.clone(), execution.clone()).await?;
            match self.storage.commit(block.clone()).await {
                Ok(()) => {}
                Err(EthStorageError::Conflict(conflicts)) => {
                    tracing::warn!(?conflicts, "storage conflict detected when saving block");
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            break (execution, block);
        };

        // notify new blocks
        if let Err(e) = self.block_notifier.send(block.clone()) {
            tracing::error!(reason = ?e, "failed to send block notification");
        };

        // notify transaction logs
        for trx in block.transactions {
            for log in trx.logs {
                if let Err(e) = self.log_notifier.send(log) {
                    tracing::error!(reason = ?e, "failed to send log notification");
                };
            }
        }

        Ok(execution)
    }

    /// Execute a function and return the function output. State changes are ignored.
    pub async fn call(&self, input: CallInput, point_in_time: StoragePointInTime) -> anyhow::Result<Execution> {
        tracing::info!(
            from = ?input.from,
            to = ?input.to,
            data_len = input.data.len(),
            data = %input.data,
            "executing read-only transaction"
        );

        let evm_input = EvmInput::from_eth_call(input, point_in_time);
        let execution = self.execute_in_evm(evm_input).await?;
        Ok(execution)
    }

    /// Submits a transaction to the EVM and awaits for its execution.
    async fn execute_in_evm(&self, evm_input: EvmInput) -> anyhow::Result<Execution> {
        let (execution_tx, execution_rx) = oneshot::channel::<anyhow::Result<Execution>>();
        self.evm_tx.send((evm_input, execution_tx))?;
        execution_rx.await?
    }

    /// Subscribe to new blocks events.
    pub fn subscribe_to_new_heads(&self) -> broadcast::Receiver<Block> {
        self.block_notifier.subscribe()
    }

    /// Subscribe to new logs events.
    pub fn subscribe_to_logs(&self) -> broadcast::Receiver<LogMined> {
        self.log_notifier.subscribe()
    }
}

// for each evm, spawn a new thread that runs in an infinite loop executing transactions.
fn spawn_background_evms(evms: NonEmpty<Box<dyn Evm>>) -> crossbeam_channel::Sender<EvmTask> {
    let (evm_tx, evm_rx) = crossbeam_channel::unbounded::<EvmTask>();

    for mut evm in evms {
        // clone shared resources for thread
        let evm_rx = evm_rx.clone();
        let tokio = Handle::current();

        // prepare thread
        let t = thread::Builder::new().name("evm".into());
        t.spawn(move || {
            // make tokio runtime available to this thread
            let _tokio_guard = tokio.enter();

            // keep executing transactions until the channel is closed
            while let Ok((input, tx)) = evm_rx.recv() {
                if let Err(e) = tx.send(evm.execute(input)) {
                    tracing::error!(reason = ?e, "failed to send evm execution result");
                };
            }
            tracing::warn!("stopping evm thread because task channel was closed");
        })
        .expect("spawning evm threads should not fail");
    }
    evm_tx
}
