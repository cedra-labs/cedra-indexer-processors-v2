// Copyright © Cedra Foundation
// SPDX-License-Identifier: Apache-2.0

// This is required because a diesel macro makes clippy sad
#![allow(clippy::extra_unused_lifetimes)]
#![allow(clippy::unused_unit)]

use crate::{
    db::resources::{COIN_ADDR, TOKEN_ADDR, TOKEN_V2_ADDR},
    parquet_processors::parquet_utils::util::{HasVersion, NamedTable},
    processors::{
        default::models::move_resources::MoveResource,
        objects::v2_object_utils::ObjectAggregatedDataMapping,
        token_v2::{
            token_models::token_utils::NAME_LENGTH, token_v2_models::v2_token_utils::DEFAULT_NONE,
        },
    },
};
use allocative_derive::Allocative;
use anyhow::Context;
use cedra_indexer_processor_sdk::{
    cedra_protos::transaction::v1::WriteResource,
    utils::convert::{standardize_address, truncate_str},
};
use field_count::FieldCount;
use parquet_derive::ParquetRecordWriter;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::error;

// PK of current_objects, i.e. object_address, resource_type
pub type CurrentTokenV2MetadataPK = (String, String);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CurrentTokenV2Metadata {
    pub object_address: String,
    pub resource_type: String,
    pub data: Value,
    pub state_key_hash: String,
    pub last_transaction_version: i64,
    pub last_transaction_timestamp: chrono::NaiveDateTime,
}

impl Ord for CurrentTokenV2Metadata {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.object_address
            .cmp(&other.object_address)
            .then(self.resource_type.cmp(&other.resource_type))
    }
}
impl PartialOrd for CurrentTokenV2Metadata {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl CurrentTokenV2Metadata {
    /// Parsing unknown resources with 0x4::token::Token
    pub fn from_write_resource(
        write_resource: &WriteResource,
        txn_version: i64,
        object_metadatas: &ObjectAggregatedDataMapping,
        txn_timestamp: chrono::NaiveDateTime,
    ) -> anyhow::Result<Option<Self>> {
        let object_address = standardize_address(&write_resource.address.to_string());
        if let Some(object_data) = object_metadatas.get(&object_address) {
            // checking if token_v2
            if object_data.token.is_some() {
                let move_tag =
                    MoveResource::convert_move_struct_tag(write_resource.r#type.as_ref().unwrap());
                let resource_type_addr = move_tag.get_address();
                if matches!(
                    resource_type_addr.as_str(),
                    COIN_ADDR | TOKEN_ADDR | TOKEN_V2_ADDR
                ) {
                    return Ok(None);
                }

                let resource = match MoveResource::from_write_resource(
                    write_resource,
                    0,
                    txn_version,
                    0,
                    txn_timestamp,
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

                let state_key_hash = object_data.object.get_state_key_hash();
                if state_key_hash != resource.state_key_hash {
                    return Ok(None);
                }

                let resource_type = truncate_str(&resource.resource_type, NAME_LENGTH);
                return Ok(Some(CurrentTokenV2Metadata {
                    object_address,
                    resource_type,
                    data: resource
                        .data
                        .context("data must be present in write resource")?,
                    state_key_hash: resource.state_key_hash,
                    last_transaction_version: txn_version,
                    last_transaction_timestamp: txn_timestamp,
                }));
            }
        }
        Ok(None)
    }
}

/// This is the parquet version of CurrentTokenV2Metadata
#[derive(
    Allocative, Clone, Debug, Default, Deserialize, FieldCount, ParquetRecordWriter, Serialize,
)]
pub struct ParquetCurrentTokenV2Metadata {
    pub object_address: String,
    pub resource_type: String,
    pub data: String,
    pub state_key_hash: String,
    pub last_transaction_version: i64,
    #[allocative(skip)]
    pub last_transaction_timestamp: chrono::NaiveDateTime,
}
impl NamedTable for ParquetCurrentTokenV2Metadata {
    const TABLE_NAME: &'static str = "current_token_v2_metadata";
}

impl HasVersion for ParquetCurrentTokenV2Metadata {
    fn version(&self) -> i64 {
        self.last_transaction_version
    }
}

impl From<CurrentTokenV2Metadata> for ParquetCurrentTokenV2Metadata {
    fn from(raw_item: CurrentTokenV2Metadata) -> Self {
        Self {
            object_address: raw_item.object_address,
            resource_type: raw_item.resource_type,
            data: canonical_json::to_string(&raw_item.data).unwrap_or_else(|_| {
                error!("Failed to serialize data to JSON: {:?}", raw_item.data);
                DEFAULT_NONE.to_string()
            }),
            state_key_hash: raw_item.state_key_hash,
            last_transaction_version: raw_item.last_transaction_version,
            last_transaction_timestamp: raw_item.last_transaction_timestamp,
        }
    }
}
