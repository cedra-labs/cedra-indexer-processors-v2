// Copyright © Cedra Foundation
// SPDX-License-Identifier: Apache-2.0

// This is required because a diesel macro makes clippy sad
#![allow(clippy::extra_unused_lifetimes)]
#![allow(clippy::unused_unit)]

use super::{
    collection_datas::{CollectionData, CurrentCollectionData},
    token_datas::{CurrentTokenData, TokenData},
    token_ownerships::{CurrentTokenOwnership, TokenOwnership},
    token_utils::{TokenResource, TokenWriteSet},
};
use crate::{
    processors::default::models::move_resources::MoveResource, schema::tokens,
    utils::counters::PROCESSOR_UNKNOWN_TYPE_COUNT,
};
use ahash::AHashMap;
use cedra_indexer_processor_sdk::{
    cedra_indexer_transaction_stream::utils::time::parse_timestamp,
    cedra_protos::transaction::v1::{
        transaction::TxnData, write_set_change::Change as WriteSetChangeEnum, DeleteTableItem,
        Transaction, WriteResource, WriteTableItem,
    },
    postgres::utils::database::DbPoolConnection,
    utils::convert::{ensure_not_negative, standardize_address},
};
use bigdecimal::{BigDecimal, Zero};
use field_count::FieldCount;
use serde::{Deserialize, Serialize};
use tracing::error;

type TableHandle = String;
type Address = String;
type TableType = String;
pub type TableHandleToOwner = AHashMap<TableHandle, TableMetadataForToken>;
pub type TokenDataIdHash = String;
// PK of current_token_ownerships, i.e. token_data_id_hash + property_version + owner_address, used to dedupe
pub type CurrentTokenOwnershipPK = (TokenDataIdHash, BigDecimal, Address);
// PK of current_token_pending_claims, i.e. token_data_id_hash + property_version + to/from_address, used to dedupe
pub type CurrentTokenPendingClaimPK = (TokenDataIdHash, BigDecimal, Address, Address);
// PK of tokens table, used to dedupe tokens
pub type TokenPK = (TokenDataIdHash, BigDecimal);

#[derive(Clone, Debug, Deserialize, FieldCount, Identifiable, Insertable, Serialize)]
#[diesel(primary_key(token_data_id_hash, property_version, transaction_version))]
#[diesel(table_name = tokens)]
pub struct Token {
    pub token_data_id_hash: String,
    pub property_version: BigDecimal,
    pub transaction_version: i64,
    pub creator_address: String,
    pub collection_name: String,
    pub name: String,
    pub token_properties: serde_json::Value,
    pub collection_data_id_hash: String,
    pub transaction_timestamp: chrono::NaiveDateTime,
}

#[derive(Debug)]
pub struct TableMetadataForToken {
    owner_address: Address,
    pub table_type: TableType,
}

impl Token {
    /// We can find token data from write sets in user transactions. Table items will contain metadata for collections
    /// and tokens. To find ownership, we have to look in write resource write sets for who owns those table handles
    ///
    /// We also will compute current versions of the token tables which are at a higher granularity than the transactional tables (only
    /// state at the last transaction will be tracked, hence using hashmap to dedupe)
    pub async fn from_transaction(
        transaction: &Transaction,
        table_handle_to_owner: &TableHandleToOwner,
        conn: &mut DbPoolConnection<'_>,
        query_retries: u32,
        query_retry_delay_ms: u64,
    ) -> (
        Vec<Self>,
        Vec<TokenOwnership>,
        Vec<TokenData>,
        Vec<CollectionData>,
        AHashMap<CurrentTokenOwnershipPK, CurrentTokenOwnership>,
        AHashMap<TokenDataIdHash, CurrentTokenData>,
        AHashMap<TokenDataIdHash, CurrentCollectionData>,
    ) {
        let txn_data = match transaction.txn_data.as_ref() {
            Some(data) => data,
            None => {
                PROCESSOR_UNKNOWN_TYPE_COUNT
                    .with_label_values(&["Token"])
                    .inc();
                tracing::warn!(
                    transaction_version = transaction.version,
                    "Transaction data doesn't exist",
                );
                return (
                    vec![],
                    vec![],
                    vec![],
                    vec![],
                    AHashMap::new(),
                    AHashMap::new(),
                    AHashMap::new(),
                );
            },
        };
        if let TxnData::User(_) = txn_data {
            let mut token_ownerships = vec![];
            let mut token_datas = vec![];
            let mut collection_datas = vec![];

            let mut tokens: AHashMap<TokenPK, Token> = AHashMap::new();
            let mut current_token_ownerships: AHashMap<
                CurrentTokenOwnershipPK,
                CurrentTokenOwnership,
            > = AHashMap::new();
            let mut current_token_datas: AHashMap<TokenDataIdHash, CurrentTokenData> =
                AHashMap::new();
            let mut current_collection_datas: AHashMap<TokenDataIdHash, CurrentCollectionData> =
                AHashMap::new();

            let txn_version = transaction.version as i64;
            let txn_timestamp =
                parse_timestamp(transaction.timestamp.as_ref().unwrap(), txn_version).naive_utc();
            let transaction_info = transaction
                .info
                .as_ref()
                .expect("Transaction info doesn't exist!");

            for wsc in &transaction_info.changes {
                // Basic token and ownership data
                let (maybe_token_w_ownership, maybe_token_data, maybe_collection_data) =
                    match wsc.change.as_ref().unwrap() {
                        WriteSetChangeEnum::WriteTableItem(write_table_item) => (
                            Self::from_write_table_item(
                                write_table_item,
                                txn_version,
                                txn_timestamp,
                                table_handle_to_owner,
                            )
                            .unwrap(),
                            TokenData::from_write_table_item(
                                write_table_item,
                                txn_version,
                                txn_timestamp,
                            )
                            .unwrap(),
                            CollectionData::from_write_table_item(
                                write_table_item,
                                txn_version,
                                txn_timestamp,
                                table_handle_to_owner,
                                conn,
                                query_retries,
                                query_retry_delay_ms,
                            )
                            .await
                            .unwrap(),
                        ),
                        WriteSetChangeEnum::DeleteTableItem(delete_table_item) => (
                            Self::from_delete_table_item(
                                delete_table_item,
                                txn_version,
                                txn_timestamp,
                                table_handle_to_owner,
                            )
                            .unwrap(),
                            None,
                            None,
                        ),
                        _ => (None, None, None),
                    };

                if let Some((token, maybe_token_ownership, maybe_current_token_ownership)) =
                    maybe_token_w_ownership
                {
                    tokens.insert(
                        (
                            token.token_data_id_hash.clone(),
                            token.property_version.clone(),
                        ),
                        token,
                    );
                    if let Some(token_ownership) = maybe_token_ownership {
                        token_ownerships.push(token_ownership);
                    }
                    if let Some(current_token_ownership) = maybe_current_token_ownership {
                        current_token_ownerships.insert(
                            (
                                current_token_ownership.token_data_id_hash.clone(),
                                current_token_ownership.property_version.clone(),
                                current_token_ownership.owner_address.clone(),
                            ),
                            current_token_ownership,
                        );
                    }
                }
                if let Some((token_data, current_token_data)) = maybe_token_data {
                    token_datas.push(token_data);
                    current_token_datas.insert(
                        current_token_data.token_data_id_hash.clone(),
                        current_token_data,
                    );
                }
                if let Some((collection_data, current_collection_data)) = maybe_collection_data {
                    collection_datas.push(collection_data);
                    current_collection_datas.insert(
                        current_collection_data.collection_data_id_hash.clone(),
                        current_collection_data,
                    );
                }
            }
            return (
                tokens.into_values().collect(),
                token_ownerships,
                token_datas,
                collection_datas,
                current_token_ownerships,
                current_token_datas,
                current_collection_datas,
            );
        }
        Default::default()
    }

    /// Get token from write table item. Table items don't have address of the table so we need to look it up in the table_handle_to_owner mapping
    /// We get the mapping from resource.
    /// If the mapping is missing we'll just leave owner address as blank. This isn't great but at least helps us account for the token
    pub fn from_write_table_item(
        table_item: &WriteTableItem,
        txn_version: i64,
        txn_timestamp: chrono::NaiveDateTime,
        table_handle_to_owner: &TableHandleToOwner,
    ) -> anyhow::Result<Option<(Self, Option<TokenOwnership>, Option<CurrentTokenOwnership>)>> {
        let table_item_data = table_item.data.as_ref().unwrap();

        let maybe_token = match TokenWriteSet::from_table_item_type(
            table_item_data.value_type.as_str(),
            &table_item_data.value,
            txn_version,
        )? {
            Some(TokenWriteSet::Token(inner)) => Some(inner),
            _ => None,
        };

        if let Some(token) = maybe_token {
            let token_id = token.id;
            let token_data_id = token_id.token_data_id;
            let collection_data_id_hash = token_data_id.get_collection_data_id_hash();
            let token_data_id_hash = token_data_id.to_hash();
            let collection_name = token_data_id.get_collection_trunc();
            let name = token_data_id.get_name_trunc();

            let token_pg = Self {
                collection_data_id_hash,
                token_data_id_hash,
                creator_address: token_data_id.get_creator_address(),
                collection_name,
                name,
                property_version: token_id.property_version,
                transaction_version: txn_version,
                token_properties: token.token_properties,
                transaction_timestamp: txn_timestamp,
            };

            let (token_ownership, current_token_ownership) = TokenOwnership::from_token(
                &token_pg,
                table_item_data.key_type.as_str(),
                &table_item_data.key,
                ensure_not_negative(token.amount),
                table_item.handle.to_string(),
                table_handle_to_owner,
            )?
            .map(|(token_ownership, current_token_ownership)| {
                (Some(token_ownership), current_token_ownership)
            })
            .unwrap_or((None, None));

            Ok(Some((token_pg, token_ownership, current_token_ownership)))
        } else {
            Ok(None)
        }
    }

    /// Get token from delete table item. The difference from write table item is that value isn't there so
    /// we'll set amount to 0 and token property to blank.
    pub fn from_delete_table_item(
        table_item: &DeleteTableItem,
        txn_version: i64,
        txn_timestamp: chrono::NaiveDateTime,
        table_handle_to_owner: &TableHandleToOwner,
    ) -> anyhow::Result<Option<(Self, Option<TokenOwnership>, Option<CurrentTokenOwnership>)>> {
        let table_item_data = table_item.data.as_ref().unwrap();

        let maybe_token_id = match TokenWriteSet::from_table_item_type(
            table_item_data.key_type.as_str(),
            &table_item_data.key,
            txn_version,
        )? {
            Some(TokenWriteSet::TokenId(inner)) => Some(inner),
            _ => None,
        };

        if let Some(token_id) = maybe_token_id {
            let token_data_id = token_id.token_data_id;
            let collection_data_id_hash = token_data_id.get_collection_data_id_hash();
            let token_data_id_hash = token_data_id.to_hash();
            let collection_name = token_data_id.get_collection_trunc();
            let name = token_data_id.get_name_trunc();

            let token = Self {
                collection_data_id_hash,
                token_data_id_hash,
                creator_address: token_data_id.get_creator_address(),
                collection_name,
                name,
                property_version: token_id.property_version,
                transaction_version: txn_version,
                token_properties: serde_json::Value::Null,
                transaction_timestamp: txn_timestamp,
            };
            let (token_ownership, current_token_ownership) = TokenOwnership::from_token(
                &token,
                table_item_data.key_type.as_str(),
                &table_item_data.key,
                BigDecimal::zero(),
                table_item.handle.to_string(),
                table_handle_to_owner,
            )?
            .map(|(token_ownership, current_token_ownership)| {
                (Some(token_ownership), current_token_ownership)
            })
            .unwrap_or((None, None));
            Ok(Some((token, token_ownership, current_token_ownership)))
        } else {
            Ok(None)
        }
    }
}

impl TableMetadataForToken {
    /// Mapping from table handle to owner type, including type of the table (AKA resource type)
    /// from user transactions in a batch of transactions
    pub fn get_table_handle_to_owner_from_transactions(
        transactions: &[Transaction],
    ) -> TableHandleToOwner {
        let mut table_handle_to_owner: TableHandleToOwner = AHashMap::new();
        // Do a first pass to get all the table metadata in the batch.
        for transaction in transactions {
            if let Some(TxnData::User(_)) = transaction.txn_data.as_ref() {
                let txn_version = transaction.version as i64;

                let transaction_info = transaction
                    .info
                    .as_ref()
                    .expect("Transaction info doesn't exist!");
                let block_timestamp =
                    parse_timestamp(transaction.timestamp.as_ref().unwrap(), txn_version)
                        .naive_utc();
                for wsc in &transaction_info.changes {
                    if let WriteSetChangeEnum::WriteResource(write_resource) =
                        wsc.change.as_ref().unwrap()
                    {
                        let maybe_map = Self::get_table_handle_to_owner(
                            write_resource,
                            txn_version,
                            block_timestamp,
                        )
                        .unwrap();
                        if let Some(map) = maybe_map {
                            table_handle_to_owner.extend(map);
                        }
                    }
                }
            }
        }
        table_handle_to_owner
    }

    /// Mapping from table handle to owner type, including type of the table (AKA resource type)
    fn get_table_handle_to_owner(
        write_resource: &WriteResource,
        txn_version: i64,
        block_timestamp: chrono::NaiveDateTime,
    ) -> anyhow::Result<Option<TableHandleToOwner>> {
        let type_str = MoveResource::get_outer_type_from_write_resource(write_resource);
        if !TokenResource::is_resource_supported(type_str.as_str()) {
            return Ok(None);
        }
        let resource = match MoveResource::from_write_resource(
            write_resource,
            0, // Placeholder, this isn't used anyway
            txn_version,
            0, // Placeholder, this isn't used anyway
            block_timestamp,
        ) {
            Ok(Some(res)) => res,
            Ok(None) => {
                error!("No resource found for transaction version {}", txn_version);
                return Ok(None);
            },
            Err(e) => {
                error!("Error processing write resource: {}", e);
                return Err(anyhow::anyhow!("Error processing write resource: {}", e));
            },
        };

        let value = TableMetadataForToken {
            owner_address: resource.resource_address.clone(),
            table_type: write_resource.type_str.clone(),
        };
        let table_handle: TableHandle = match TokenResource::from_resource(
            &type_str,
            resource.data.as_ref().unwrap(),
            txn_version,
        )? {
            TokenResource::CollectionResource(collection_resource) => {
                collection_resource.collection_data.get_handle()
            },
            TokenResource::TokenStoreResource(inner) => inner.tokens.get_handle(),
            TokenResource::PendingClaimsResource(inner) => inner.pending_claims.get_handle(),
        };
        Ok(Some(AHashMap::from([(
            standardize_address(&table_handle),
            value,
        )])))
    }

    pub fn get_owner_address(&self) -> String {
        standardize_address(&self.owner_address)
    }
}
