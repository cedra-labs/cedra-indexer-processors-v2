// Copyright © Cedra Foundation
// SPDX-License-Identifier: Apache-2.0

// This is required because a diesel macro makes clippy sad
#![allow(clippy::extra_unused_lifetimes)]
#![allow(clippy::unused_unit)]

use crate::{
    processors::default::models::table_items::{PostgresTableItem, TableItem},
    schema::coin_supply,
};
use anyhow::Context;
use cedra_indexer_processor_sdk::{
    cedra_protos::transaction::v1::WriteTableItem,
    utils::{constants::CEDRA_COIN_TYPE_STR, extract::hash_str},
};
use bigdecimal::BigDecimal;
use field_count::FieldCount;
use serde::{Deserialize, Serialize};
const CEDRA_COIN_SUPPLY_TABLE_HANDLE: &str =
    "0x1b854694ae746cdbd8d44186ca4929b2b337df21d1c74633be19b2710552fdca";
const CEDRA_COIN_SUPPLY_TABLE_KEY: &str =
    "0x619dc29a0aac8fa146714058e8dd6d2d0f3bdf5f6331907bf91f3acd81e6935";

#[derive(Clone, Debug, Deserialize, FieldCount, Identifiable, Insertable, Serialize)]
#[diesel(primary_key(transaction_version, coin_type_hash))]
#[diesel(table_name = coin_supply)]
pub struct CoinSupply {
    pub transaction_version: i64,
    pub coin_type_hash: String,
    pub coin_type: String,
    pub supply: BigDecimal,
    pub transaction_timestamp: chrono::NaiveDateTime,
    pub transaction_epoch: i64,
}

impl CoinSupply {
    /// Currently only supports cedra_coin. Aggregator table detail is in CoinInfo which for cedra coin appears during genesis.
    /// We query for the aggregator table details (handle and key) once upon indexer initiation and use it to fetch supply.
    pub fn from_write_table_item(
        write_table_item: &WriteTableItem,
        txn_version: i64,
        txn_timestamp: chrono::NaiveDateTime,
        txn_epoch: i64,
    ) -> anyhow::Result<Option<Self>> {
        if let Some(data) = &write_table_item.data {
            // Return early if not aggregator table type
            if !(data.key_type == "address" && data.value_type == "u128") {
                return Ok(None);
            }
            // Return early if not aggregator table handle
            if write_table_item.handle.as_str() != CEDRA_COIN_SUPPLY_TABLE_HANDLE {
                return Ok(None);
            }

            let table_item = {
                let (table_item, _) = TableItem::from_write_table_item(
                    write_table_item,
                    0,
                    txn_version,
                    0,
                    txn_timestamp,
                );
                PostgresTableItem::from(table_item)
            };

            // Return early if not cedra coin aggregator key
            let table_key = table_item.decoded_key.as_str().unwrap();
            if table_key != CEDRA_COIN_SUPPLY_TABLE_KEY {
                return Ok(None);
            }
            // Everything matches. Get the coin supply
            let supply = table_item
                .decoded_value
                .as_ref()
                .unwrap()
                .as_str()
                .unwrap()
                .parse::<BigDecimal>()
                .context(format!(
                    "cannot parse string as u128: {:?}, version {}",
                    table_item.decoded_value.as_ref(),
                    txn_version
                ))?;
            return Ok(Some(Self {
                transaction_version: txn_version,
                coin_type_hash: hash_str(CEDRA_COIN_TYPE_STR),
                coin_type: CEDRA_COIN_TYPE_STR.to_string(),
                supply,
                transaction_timestamp: txn_timestamp,
                transaction_epoch: txn_epoch,
            }));
        }
        Ok(None)
    }
}
