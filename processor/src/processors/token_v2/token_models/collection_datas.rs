// Copyright © Cedra Foundation
// SPDX-License-Identifier: Apache-2.0

// This is required because a diesel macro makes clippy sad
#![allow(clippy::extra_unused_lifetimes)]
#![allow(clippy::unused_unit)]

use super::{
    token_utils::{CollectionDataIdType, TokenWriteSet},
    tokens::TableHandleToOwner,
};
use crate::schema::{collection_datas, current_collection_datas};
use cedra_indexer_processor_sdk::{
    cedra_protos::transaction::v1::WriteTableItem, postgres::utils::database::DbPoolConnection,
    utils::convert::standardize_address,
};
use bigdecimal::BigDecimal;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use field_count::FieldCount;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, FieldCount, Identifiable, Insertable, Serialize)]
#[diesel(primary_key(collection_data_id_hash, transaction_version))]
#[diesel(table_name = collection_datas)]
pub struct CollectionData {
    pub collection_data_id_hash: String,
    pub transaction_version: i64,
    pub creator_address: String,
    pub collection_name: String,
    pub description: String,
    pub metadata_uri: String,
    pub supply: BigDecimal,
    pub maximum: BigDecimal,
    pub maximum_mutable: bool,
    pub uri_mutable: bool,
    pub description_mutable: bool,
    pub table_handle: String,
    pub transaction_timestamp: chrono::NaiveDateTime,
}

#[derive(Clone, Debug, Deserialize, FieldCount, Identifiable, Insertable, Serialize)]
#[diesel(primary_key(collection_data_id_hash))]
#[diesel(table_name = current_collection_datas)]
pub struct CurrentCollectionData {
    pub collection_data_id_hash: String,
    pub creator_address: String,
    pub collection_name: String,
    pub description: String,
    pub metadata_uri: String,
    pub supply: BigDecimal,
    pub maximum: BigDecimal,
    pub maximum_mutable: bool,
    pub uri_mutable: bool,
    pub description_mutable: bool,
    pub last_transaction_version: i64,
    pub table_handle: String,
    pub last_transaction_timestamp: chrono::NaiveDateTime,
}

/// Need a separate struct for queryable because we don't want to define the inserted_at column (letting DB fill)
#[derive(Debug, Identifiable, Queryable)]
#[diesel(primary_key(collection_data_id_hash))]
#[diesel(table_name = current_collection_datas)]
pub struct CurrentCollectionDataQuery {
    pub collection_data_id_hash: String,
    pub creator_address: String,
    pub collection_name: String,
    pub description: String,
    pub metadata_uri: String,
    pub supply: BigDecimal,
    pub maximum: BigDecimal,
    pub maximum_mutable: bool,
    pub uri_mutable: bool,
    pub description_mutable: bool,
    pub last_transaction_version: i64,
    pub inserted_at: chrono::NaiveDateTime,
    pub table_handle: String,
    pub last_transaction_timestamp: chrono::NaiveDateTime,
}

impl CollectionData {
    pub async fn from_write_table_item(
        table_item: &WriteTableItem,
        txn_version: i64,
        txn_timestamp: chrono::NaiveDateTime,
        table_handle_to_owner: &TableHandleToOwner,
        conn: &mut DbPoolConnection<'_>,
        query_retries: u32,
        query_retry_delay_ms: u64,
    ) -> anyhow::Result<Option<(Self, CurrentCollectionData)>> {
        let table_item_data = table_item.data.as_ref().unwrap();

        let maybe_collection_data = match TokenWriteSet::from_table_item_type(
            table_item_data.value_type.as_str(),
            &table_item_data.value,
            txn_version,
        )? {
            Some(TokenWriteSet::CollectionData(inner)) => Some(inner),
            _ => None,
        };
        if let Some(collection_data) = maybe_collection_data {
            let table_handle = table_item.handle.to_string();
            let maybe_creator_address = table_handle_to_owner
                .get(&standardize_address(&table_handle))
                .map(|table_metadata| table_metadata.get_owner_address());
            let mut creator_address = match maybe_creator_address {
                Some(ca) => ca,
                None => match Self::get_collection_creator(
                    conn,
                    &table_handle,
                    query_retries,
                    query_retry_delay_ms,
                )
                .await
                {
                    Ok(creator) => creator,
                    Err(_) => {
                        tracing::error!(
                            transaction_version = txn_version,
                            lookup_key = &table_handle,
                            "Failed to get collection creator for table handle. You probably should backfill db."
                        );
                        return Ok(None);
                    },
                },
            };
            creator_address = standardize_address(&creator_address);
            let collection_data_id =
                CollectionDataIdType::new(creator_address, collection_data.get_name().to_string());
            let collection_data_id_hash = collection_data_id.to_hash();
            let collection_name = collection_data.get_name_trunc();
            let metadata_uri = collection_data.get_uri_trunc();

            Ok(Some((
                Self {
                    collection_data_id_hash: collection_data_id_hash.clone(),
                    collection_name: collection_name.clone(),
                    creator_address: collection_data_id.creator.clone(),
                    description: collection_data.description.clone(),
                    transaction_version: txn_version,
                    metadata_uri: metadata_uri.clone(),
                    supply: collection_data.supply.clone(),
                    maximum: collection_data.maximum.clone(),
                    maximum_mutable: collection_data.mutability_config.maximum,
                    uri_mutable: collection_data.mutability_config.uri,
                    description_mutable: collection_data.mutability_config.description,
                    table_handle: table_handle.clone(),
                    transaction_timestamp: txn_timestamp,
                },
                CurrentCollectionData {
                    collection_data_id_hash,
                    collection_name,
                    creator_address: collection_data_id.creator,
                    description: collection_data.description,
                    metadata_uri,
                    supply: collection_data.supply,
                    maximum: collection_data.maximum,
                    maximum_mutable: collection_data.mutability_config.maximum,
                    uri_mutable: collection_data.mutability_config.uri,
                    description_mutable: collection_data.mutability_config.description,
                    last_transaction_version: txn_version,
                    table_handle,
                    last_transaction_timestamp: txn_timestamp,
                },
            )))
        } else {
            Ok(None)
        }
    }

    /// If collection data is not in resources of the same transaction, then try looking for it in the database. Since collection owner
    /// cannot change, we can just look in the current_collection_datas table.
    /// Retrying a few times since this collection could've been written in a separate thread.
    pub async fn get_collection_creator(
        conn: &mut DbPoolConnection<'_>,
        table_handle: &str,
        query_retries: u32,
        query_retry_delay_ms: u64,
    ) -> anyhow::Result<String> {
        let mut tried = 0;
        while tried < query_retries {
            tried += 1;
            match CurrentCollectionDataQuery::get_by_table_handle(conn, table_handle).await {
                Ok(current_collection_data) => return Ok(current_collection_data.creator_address),
                Err(_) => {
                    if tried < query_retries {
                        tokio::time::sleep(std::time::Duration::from_millis(query_retry_delay_ms))
                            .await;
                    }
                },
            }
        }
        Err(anyhow::anyhow!("Failed to get collection creator"))
    }
}

impl CurrentCollectionDataQuery {
    pub async fn get_by_table_handle(
        conn: &mut DbPoolConnection<'_>,
        table_handle: &str,
    ) -> diesel::QueryResult<Self> {
        current_collection_datas::table
            .filter(current_collection_datas::table_handle.eq(table_handle))
            .first::<Self>(conn)
            .await
    }
}
