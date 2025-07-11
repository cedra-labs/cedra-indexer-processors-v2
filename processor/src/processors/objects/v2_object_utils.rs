// Copyright © Cedra Foundation
// SPDX-License-Identifier: Apache-2.0

// This is required because a diesel macro makes clippy sad
#![allow(clippy::extra_unused_lifetimes)]
#![allow(clippy::unused_unit)]

use crate::{
    db::resources::FromWriteResource,
    processors::{
        fungible_asset::fungible_asset_models::v2_fungible_asset_utils::{
            ConcurrentFungibleAssetBalance, ConcurrentFungibleAssetSupply, FungibleAssetMetadata,
            FungibleAssetStore, FungibleAssetSupply,
        },
        token_v2::token_v2_models::v2_token_utils::{
            CedraCollection, ConcurrentSupply, FixedSupply, PropertyMapModel, TokenIdentifiers,
            TokenV2, TransferEvent, UnlimitedSupply,
        },
    },
};
use ahash::AHashMap;
use cedra_indexer_processor_sdk::{
    cedra_protos::transaction::v1::WriteResource,
    utils::convert::{deserialize_from_string, standardize_address},
};
use bigdecimal::BigDecimal;
use serde::{Deserialize, Serialize};
// PK of current_objects, i.e. object_address
pub type CurrentObjectPK = String;

/// Tracks all object related metadata in a hashmap for quick access (keyed on address of the object)
pub type ObjectAggregatedDataMapping = AHashMap<CurrentObjectPK, ObjectAggregatedData>;

/// Index of the event so that we can write its inverse to the db as primary key (to avoid collisiona)
pub type EventIndex = i64;

/// This contains metadata for the object. This only includes fungible asset and token v2 metadata for now.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ObjectAggregatedData {
    pub object: ObjectWithMetadata,
    // There could be more than one transfers on the same transaction
    pub transfer_events: Vec<(EventIndex, TransferEvent)>,
    // This would make transfers impossible
    pub untransferable: Option<Untransferable>,
    // Fungible asset structs
    pub fungible_asset_metadata: Option<FungibleAssetMetadata>,
    pub fungible_asset_supply: Option<FungibleAssetSupply>,
    pub concurrent_fungible_asset_supply: Option<ConcurrentFungibleAssetSupply>,
    pub fungible_asset_store: Option<FungibleAssetStore>,
    pub concurrent_fungible_asset_balance: Option<ConcurrentFungibleAssetBalance>,
    // Token v2 structs
    pub cedra_collection: Option<CedraCollection>,
    pub fixed_supply: Option<FixedSupply>,
    pub property_map: Option<PropertyMapModel>,
    pub token: Option<TokenV2>,
    pub unlimited_supply: Option<UnlimitedSupply>,
    pub concurrent_supply: Option<ConcurrentSupply>,
    pub token_identifier: Option<TokenIdentifiers>,
}

impl Default for ObjectAggregatedData {
    fn default() -> Self {
        Self {
            object: ObjectWithMetadata {
                object_core: ObjectCore {
                    allow_ungated_transfer: false,
                    guid_creation_num: BigDecimal::default(),
                    owner: String::default(),
                },
                state_key_hash: String::default(),
            },
            transfer_events: Vec::new(),
            untransferable: None,
            fungible_asset_metadata: None,
            fungible_asset_supply: None,
            concurrent_fungible_asset_supply: None,
            concurrent_fungible_asset_balance: None,
            fungible_asset_store: None,
            cedra_collection: None,
            fixed_supply: None,
            property_map: None,
            token: None,
            unlimited_supply: None,
            concurrent_supply: None,
            token_identifier: None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ObjectCore {
    pub allow_ungated_transfer: bool,
    #[serde(deserialize_with = "deserialize_from_string")]
    pub guid_creation_num: BigDecimal,
    owner: String,
}

impl ObjectCore {
    pub fn get_owner_address(&self) -> String {
        standardize_address(&self.owner)
    }
}

impl TryFrom<&WriteResource> for ObjectCore {
    type Error = anyhow::Error;

    fn try_from(write_resource: &WriteResource) -> anyhow::Result<Self> {
        serde_json::from_str(write_resource.data.as_str()).map_err(anyhow::Error::msg)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ObjectWithMetadata {
    pub object_core: ObjectCore,
    pub state_key_hash: String,
}

impl FromWriteResource<'_> for ObjectWithMetadata {
    fn from_write_resource(write_resource: &WriteResource) -> anyhow::Result<Option<Self>> {
        Ok(
            ObjectCore::from_write_resource(write_resource)?.map(|object_core| {
                let state_key_hash = standardize_address(
                    hex::encode(write_resource.state_key_hash.as_slice()).as_str(),
                );
                Self {
                    object_core,
                    state_key_hash,
                }
            }),
        )
    }
}

impl ObjectWithMetadata {
    pub fn get_state_key_hash(&self) -> String {
        standardize_address(&self.state_key_hash)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Untransferable {
    dummy_field: bool,
}

impl TryFrom<&WriteResource> for Untransferable {
    type Error = anyhow::Error;

    fn try_from(write_resource: &WriteResource) -> anyhow::Result<Self> {
        serde_json::from_str(write_resource.data.as_str()).map_err(anyhow::Error::msg)
    }
}
