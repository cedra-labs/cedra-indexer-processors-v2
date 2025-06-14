// Copyright © Cedra Foundation
// SPDX-License-Identifier: Apache-2.0

// This is required because a diesel macro makes clippy sad
#![allow(clippy::extra_unused_lifetimes)]
#![allow(clippy::unused_unit)]

use super::write_set_changes::{WriteSetChangeDetail, WriteSetChangeModel};
use crate::{
    parquet_processors::parquet_utils::util::{HasVersion, NamedTable},
    utils::counters::PROCESSOR_UNKNOWN_TYPE_COUNT,
};
use allocative_derive::Allocative;
use cedra_indexer_processor_sdk::{
    cedra_protos::transaction::v1::{
        transaction::{TransactionType, TxnData},
        Transaction as TransactionPB, TransactionInfo, TransactionSizeInfo,
    },
    utils::{
        convert::standardize_address,
        extract::{get_clean_payload, get_clean_writeset, get_payload_type},
    },
};
use field_count::FieldCount;
use parquet_derive::ParquetRecordWriter;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Transaction {
    pub txn_version: i64,
    pub block_height: i64,
    pub epoch: i64,
    pub txn_type: String,
    pub payload: Option<String>,
    pub payload_type: Option<String>,
    pub gas_used: u64,
    pub success: bool,
    pub vm_status: String,
    pub num_events: i64,
    pub num_write_set_changes: i64,
    pub txn_hash: String,
    pub state_change_hash: String,
    pub event_root_hash: String,
    pub state_checkpoint_hash: Option<String>,
    pub accumulator_root_hash: String,
    pub txn_total_bytes: i64,
    pub block_timestamp: chrono::NaiveDateTime,
}

impl Transaction {
    fn from_transaction_info(
        info: &TransactionInfo,
        txn_version: i64,
        epoch: i64,
        block_height: i64,
    ) -> Self {
        Self {
            txn_version,
            block_height,
            txn_hash: standardize_address(hex::encode(info.hash.as_slice()).as_str()),
            state_change_hash: standardize_address(
                hex::encode(info.state_change_hash.as_slice()).as_str(),
            ),
            event_root_hash: standardize_address(
                hex::encode(info.event_root_hash.as_slice()).as_str(),
            ),
            state_checkpoint_hash: info
                .state_checkpoint_hash
                .as_ref()
                .map(|hash| standardize_address(hex::encode(hash).as_str())),
            gas_used: info.gas_used,
            success: info.success,
            vm_status: info.vm_status.clone(),
            accumulator_root_hash: standardize_address(
                hex::encode(info.accumulator_root_hash.as_slice()).as_str(),
            ),
            num_write_set_changes: info.changes.len() as i64,
            epoch,
            ..Default::default()
        }
    }

    fn from_transaction_info_with_data(
        info: &TransactionInfo,
        payload: Option<String>,
        payload_type: Option<String>,
        txn_version: i64,
        txn_type: String,
        num_events: i64,
        block_height: i64,
        epoch: i64,
        block_timestamp: chrono::NaiveDateTime,
        txn_size_info: Option<&TransactionSizeInfo>,
    ) -> Self {
        Self {
            txn_type,
            payload,
            txn_version,
            block_height,
            txn_hash: standardize_address(hex::encode(info.hash.as_slice()).as_str()),
            state_change_hash: standardize_address(
                hex::encode(info.state_change_hash.as_slice()).as_str(),
            ),
            event_root_hash: standardize_address(
                hex::encode(info.event_root_hash.as_slice()).as_str(),
            ),
            state_checkpoint_hash: info
                .state_checkpoint_hash
                .as_ref()
                .map(|hash| standardize_address(hex::encode(hash).as_str())),
            gas_used: info.gas_used,
            success: info.success,
            vm_status: info.vm_status.clone(),
            accumulator_root_hash: standardize_address(
                hex::encode(info.accumulator_root_hash.as_slice()).as_str(),
            ),
            num_events,
            num_write_set_changes: info.changes.len() as i64,
            epoch,
            payload_type,
            txn_total_bytes: txn_size_info
                .map_or(0, |size_info| size_info.transaction_bytes as i64),
            block_timestamp,
        }
    }

    pub fn from_transaction(
        transaction: &TransactionPB,
    ) -> (Self, Vec<WriteSetChangeModel>, Vec<WriteSetChangeDetail>) {
        let block_height = transaction.block_height as i64;
        let epoch = transaction.epoch as i64;
        let transaction_info = transaction
            .info
            .as_ref()
            .expect("Transaction info doesn't exist!");
        let txn_data = match transaction.txn_data.as_ref() {
            Some(txn_data) => txn_data,
            None => {
                PROCESSOR_UNKNOWN_TYPE_COUNT
                    .with_label_values(&["Transaction"])
                    .inc();
                tracing::warn!(
                    transaction_version = transaction.version,
                    "Transaction data doesn't exist",
                );
                let transaction_out = Self::from_transaction_info(
                    transaction_info,
                    transaction.version as i64,
                    epoch,
                    block_height,
                );
                return (transaction_out, Vec::new(), Vec::new());
            },
        };
        let txn_version = transaction.version as i64;
        let transaction_type = TransactionType::try_from(transaction.r#type)
            .expect("Transaction type doesn't exist!")
            .as_str_name()
            .to_string();
        let timestamp = transaction
            .timestamp
            .as_ref()
            .expect("Transaction timestamp doesn't exist!");
        #[allow(deprecated)]
        let block_timestamp = chrono::NaiveDateTime::from_timestamp_opt(timestamp.seconds, 0)
            .expect("Txn Timestamp is invalid!");

        let txn_size_info = transaction.size_info.as_ref();

        match txn_data {
            TxnData::User(user_txn) => {
                let (wsc, wsc_detail) = WriteSetChangeModel::from_write_set_changes(
                    &transaction_info.changes,
                    txn_version,
                    block_height,
                    block_timestamp,
                );
                let request = &user_txn
                    .request
                    .as_ref()
                    .expect("Getting user request failed.");

                let (payload_cleaned, payload_type) = match request.payload.as_ref() {
                    Some(payload) => {
                        let payload_cleaned = get_clean_payload(payload, txn_version);
                        (payload_cleaned, Some(get_payload_type(payload)))
                    },
                    None => (None, None),
                };

                let serialized_payload =
                    payload_cleaned.map(|payload| canonical_json::to_string(&payload).unwrap());
                (
                    Self::from_transaction_info_with_data(
                        transaction_info,
                        serialized_payload,
                        payload_type,
                        txn_version,
                        transaction_type,
                        user_txn.events.len() as i64,
                        block_height,
                        epoch,
                        block_timestamp,
                        txn_size_info,
                    ),
                    wsc,
                    wsc_detail,
                )
            },
            TxnData::Genesis(genesis_txn) => {
                let (wsc, wsc_detail) = WriteSetChangeModel::from_write_set_changes(
                    &transaction_info.changes,
                    txn_version,
                    block_height,
                    block_timestamp,
                );
                let payload = genesis_txn.payload.as_ref().unwrap();
                let payload_cleaned = get_clean_writeset(payload, txn_version);
                // It's genesis so no big deal
                // let serialized_payload = serde_json::to_string(&payload_cleaned).unwrap(); // Handle errors as needed
                let serialized_payload =
                    payload_cleaned.map(|payload| canonical_json::to_string(&payload).unwrap());

                let payload_type = None;
                (
                    Self::from_transaction_info_with_data(
                        transaction_info,
                        serialized_payload,
                        payload_type,
                        txn_version,
                        transaction_type,
                        genesis_txn.events.len() as i64,
                        block_height,
                        epoch,
                        block_timestamp,
                        txn_size_info,
                    ),
                    wsc,
                    wsc_detail,
                )
            },
            TxnData::BlockMetadata(block_metadata_txn) => {
                let (wsc, wsc_detail) = WriteSetChangeModel::from_write_set_changes(
                    &transaction_info.changes,
                    txn_version,
                    block_height,
                    block_timestamp,
                );
                (
                    Self::from_transaction_info_with_data(
                        transaction_info,
                        None,
                        None,
                        txn_version,
                        transaction_type,
                        block_metadata_txn.events.len() as i64,
                        block_height,
                        epoch,
                        block_timestamp,
                        txn_size_info,
                    ),
                    wsc,
                    wsc_detail,
                )
            },
            TxnData::StateCheckpoint(_) => (
                Self::from_transaction_info_with_data(
                    transaction_info,
                    None,
                    None,
                    txn_version,
                    transaction_type,
                    0,
                    block_height,
                    epoch,
                    block_timestamp,
                    txn_size_info,
                ),
                vec![],
                vec![],
            ),
            TxnData::Validator(inner) => {
                let (wsc, wsc_detail) = WriteSetChangeModel::from_write_set_changes(
                    &transaction_info.changes,
                    txn_version,
                    block_height,
                    block_timestamp,
                );
                (
                    Self::from_transaction_info_with_data(
                        transaction_info,
                        None,
                        None,
                        txn_version,
                        transaction_type,
                        inner.events.len() as i64,
                        block_height,
                        epoch,
                        block_timestamp,
                        txn_size_info,
                    ),
                    wsc,
                    wsc_detail,
                )
            },
            TxnData::BlockEpilogue(_) => (
                Self::from_transaction_info_with_data(
                    transaction_info,
                    None,
                    None,
                    txn_version,
                    transaction_type,
                    0,
                    block_height,
                    epoch,
                    block_timestamp,
                    txn_size_info,
                ),
                vec![],
                vec![],
            ),
        }
    }

    pub fn from_transactions(
        transactions: &[TransactionPB],
    ) -> (
        Vec<Self>,
        Vec<WriteSetChangeModel>,
        Vec<WriteSetChangeDetail>,
    ) {
        let mut txns = vec![];
        let mut wscs = vec![];
        let mut wsc_details = vec![];

        for txn in transactions {
            let (txn, mut wsc_list, mut wsc_detail_list) = Self::from_transaction(txn);
            txns.push(txn.clone());

            wscs.append(&mut wsc_list);

            wsc_details.append(&mut wsc_detail_list);
        }
        (txns, wscs, wsc_details)
    }
}

// Prevent conflicts with other things named `Transaction`
pub type TransactionModel = Transaction;

#[derive(
    Allocative, Clone, Debug, Default, Deserialize, FieldCount, Serialize, ParquetRecordWriter,
)]
pub struct ParquetTransaction {
    pub txn_version: i64,
    pub block_height: i64,
    pub epoch: i64,
    pub txn_type: String,
    pub payload: Option<String>,
    pub payload_type: Option<String>,
    pub gas_used: u64,
    pub success: bool,
    pub vm_status: String,
    pub num_events: i64,
    pub num_write_set_changes: i64,
    pub txn_hash: String,
    pub state_change_hash: String,
    pub event_root_hash: String,
    pub state_checkpoint_hash: Option<String>,
    pub accumulator_root_hash: String,
    pub txn_total_bytes: i64,
    #[allocative(skip)]
    pub block_timestamp: chrono::NaiveDateTime,
}

// TODO: revisit and remove this if we can.
impl NamedTable for ParquetTransaction {
    const TABLE_NAME: &'static str = "transactions";
}

// TODO: revisit and remove this if we can.
impl HasVersion for ParquetTransaction {
    fn version(&self) -> i64 {
        self.txn_version
    }
}

impl From<Transaction> for ParquetTransaction {
    fn from(transaction: Transaction) -> Self {
        ParquetTransaction {
            txn_version: transaction.txn_version,
            block_height: transaction.block_height,
            epoch: transaction.epoch,
            txn_type: transaction.txn_type,
            payload: transaction.payload,
            payload_type: transaction.payload_type,
            gas_used: transaction.gas_used,
            success: transaction.success,
            vm_status: transaction.vm_status,
            num_events: transaction.num_events,
            num_write_set_changes: transaction.num_write_set_changes,
            txn_hash: transaction.txn_hash,
            state_change_hash: transaction.state_change_hash,
            event_root_hash: transaction.event_root_hash,
            state_checkpoint_hash: transaction.state_checkpoint_hash,
            accumulator_root_hash: transaction.accumulator_root_hash,
            txn_total_bytes: transaction.txn_total_bytes,
            block_timestamp: transaction.block_timestamp,
        }
    }
}
