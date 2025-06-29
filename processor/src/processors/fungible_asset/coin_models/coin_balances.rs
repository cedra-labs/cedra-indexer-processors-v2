// Copyright © Cedra Foundation
// SPDX-License-Identifier: Apache-2.0

// This is required because a diesel macro makes clippy sad
#![allow(clippy::extra_unused_lifetimes)]
#![allow(clippy::unused_unit)]

use super::coin_utils::{CoinInfoType, CoinResource};
use crate::{
    processors::fungible_asset::fungible_asset_models::v2_fungible_asset_activities::EventToCoinType,
    schema::{coin_balances, current_coin_balances},
};
use ahash::AHashMap;
use cedra_indexer_processor_sdk::{
    cedra_protos::transaction::v1::WriteResource, utils::convert::standardize_address,
};
use bigdecimal::BigDecimal;
use field_count::FieldCount;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, FieldCount, Identifiable, Insertable, Serialize)]
#[diesel(primary_key(transaction_version, owner_address, coin_type))]
#[diesel(table_name = coin_balances)]
pub struct CoinBalance {
    pub transaction_version: i64,
    pub owner_address: String,
    pub coin_type_hash: String,
    pub coin_type: String,
    pub amount: BigDecimal,
    pub transaction_timestamp: chrono::NaiveDateTime,
}

#[derive(
    Clone, Debug, Deserialize, FieldCount, Identifiable, Insertable, Serialize, Selectable,
)]
#[diesel(primary_key(owner_address, coin_type))]
#[diesel(table_name = current_coin_balances)]
pub struct CurrentCoinBalance {
    pub owner_address: String,
    pub coin_type_hash: String,
    pub coin_type: String,
    pub amount: BigDecimal,
    pub last_transaction_version: i64,
    pub last_transaction_timestamp: chrono::NaiveDateTime,
}

impl CoinBalance {
    /// Getting coin balances from resources
    pub fn from_write_resource(
        write_resource: &WriteResource,
        txn_version: i64,
        txn_timestamp: chrono::NaiveDateTime,
        wsc_index: i64,
    ) -> anyhow::Result<Option<(Self, CurrentCoinBalance, EventToCoinType)>> {
        match &CoinResource::from_write_resource(write_resource, txn_version, txn_timestamp)? {
            Some(CoinResource::CoinStoreResource(inner)) => {
                let coin_info_type = &CoinInfoType::from_move_type(
                    &write_resource.r#type.as_ref().unwrap().generic_type_params[0],
                    write_resource.type_str.as_ref(),
                    txn_version,
                    wsc_index,
                );
                let owner_address = standardize_address(write_resource.address.as_str());
                let coin_balance = Self {
                    transaction_version: txn_version,
                    owner_address: owner_address.clone(),
                    coin_type_hash: coin_info_type.to_hash(),
                    coin_type: coin_info_type.get_coin_type_trunc(),
                    amount: inner.coin.value.clone(),
                    transaction_timestamp: txn_timestamp,
                };
                let current_coin_balance = CurrentCoinBalance {
                    owner_address,
                    coin_type_hash: coin_info_type.to_hash(),
                    coin_type: coin_info_type.get_coin_type_trunc(),
                    amount: inner.coin.value.clone(),
                    last_transaction_version: txn_version,
                    last_transaction_timestamp: txn_timestamp,
                };
                let event_to_coin_mapping: EventToCoinType = AHashMap::from([
                    (
                        inner.withdraw_events.guid.id.get_standardized(),
                        coin_balance.coin_type.clone(),
                    ),
                    (
                        inner.deposit_events.guid.id.get_standardized(),
                        coin_balance.coin_type.clone(),
                    ),
                ]);
                Ok(Some((
                    coin_balance,
                    current_coin_balance,
                    event_to_coin_mapping,
                )))
            },
            _ => Ok(None),
        }
    }
}
