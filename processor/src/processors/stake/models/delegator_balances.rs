// Copyright © Cedra Foundation

// This is required because a diesel macro makes clippy sad
#![allow(clippy::extra_unused_lifetimes)]
use crate::{
    parquet_processors::parquet_utils::util::{HasVersion, NamedTable},
    processors::{
        default::models::table_items::{PostgresTableItem, TableItem},
        stake::models::delegator_pools::{
            DelegatorPool, DelegatorPoolBalanceMetadata, PoolBalanceMetadata,
        },
    },
    schema::{current_delegator_balances, delegator_balances},
};
use ahash::AHashMap;
use allocative::Allocative;
use anyhow::Context;
use cedra_indexer_processor_sdk::{
    cedra_indexer_transaction_stream::utils::time::parse_timestamp,
    cedra_protos::transaction::v1::{
        write_set_change::Change, DeleteTableItem, Transaction, WriteResource, WriteTableItem,
    },
    postgres::utils::database::DbPoolConnection,
    utils::convert::standardize_address,
};
use bigdecimal::{BigDecimal, Zero};
use chrono::NaiveDateTime;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use field_count::FieldCount;
use parquet_derive::ParquetRecordWriter;
use serde::{Deserialize, Serialize};

pub type TableHandle = String;
pub type Address = String;
pub type ShareToStakingPoolMapping = AHashMap<TableHandle, DelegatorPoolBalanceMetadata>;
pub type ShareToPoolMapping = AHashMap<TableHandle, PoolBalanceMetadata>;
pub type CurrentDelegatorBalancePK = (Address, Address, String);
pub type CurrentDelegatorBalanceMap = AHashMap<CurrentDelegatorBalancePK, CurrentDelegatorBalance>;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CurrentDelegatorBalance {
    pub delegator_address: String,
    pub pool_address: String,
    pub pool_type: String,
    pub table_handle: String,
    pub last_transaction_version: i64,
    pub shares: BigDecimal,
    pub parent_table_handle: String,
    pub block_timestamp: chrono::NaiveDateTime,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DelegatorBalance {
    pub transaction_version: i64,
    pub write_set_change_index: i64,
    pub delegator_address: String,
    pub pool_address: String,
    pub pool_type: String,
    pub table_handle: String,
    pub shares: BigDecimal,
    pub parent_table_handle: String,
    pub block_timestamp: chrono::NaiveDateTime,
}

#[derive(Debug, Identifiable, Queryable)]
#[diesel(primary_key(delegator_address, pool_address, pool_type))]
#[diesel(table_name = current_delegator_balances)]
pub struct CurrentDelegatorBalanceQuery {
    pub delegator_address: String,
    pub pool_address: String,
    pub pool_type: String,
    pub table_handle: String,
    pub last_transaction_version: i64,
    pub inserted_at: chrono::NaiveDateTime,
    pub shares: BigDecimal,
    pub parent_table_handle: String,
}

impl CurrentDelegatorBalance {
    /// Getting active share balances. Only 1 active pool per staking pool tracked in a single table
    pub async fn get_active_share_from_write_table_item(
        write_table_item: &WriteTableItem,
        txn_version: i64,
        write_set_change_index: i64,
        active_pool_to_staking_pool: &ShareToStakingPoolMapping,
        block_timestamp: NaiveDateTime,
    ) -> anyhow::Result<Option<(DelegatorBalance, Self)>> {
        let table_handle = standardize_address(&write_table_item.handle.to_string());
        // The mapping will tell us if the table item is an active share table
        if let Some(pool_balance) = active_pool_to_staking_pool.get(&table_handle) {
            let pool_address = pool_balance.staking_pool_address.clone();
            let delegator_address = standardize_address(&write_table_item.key.to_string());

            // Convert to TableItem model. Some fields are just placeholders
            let table_item = {
                let (base_table_item, _) = TableItem::from_write_table_item(
                    write_table_item,
                    0,
                    txn_version,
                    0,
                    block_timestamp,
                );
                PostgresTableItem::from(base_table_item)
            };

            let shares: BigDecimal = table_item
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
            let shares = shares / &pool_balance.scaling_factor;
            Ok(Some((
                DelegatorBalance {
                    transaction_version: txn_version,
                    write_set_change_index,
                    delegator_address: delegator_address.clone(),
                    pool_address: pool_address.clone(),
                    pool_type: "active_shares".to_string(),
                    table_handle: table_handle.clone(),
                    shares: shares.clone(),
                    parent_table_handle: table_handle.clone(),
                    block_timestamp,
                },
                Self {
                    delegator_address,
                    pool_address,
                    pool_type: "active_shares".to_string(),
                    table_handle: table_handle.clone(),
                    last_transaction_version: txn_version,
                    shares,
                    parent_table_handle: table_handle,
                    block_timestamp,
                },
            )))
        } else {
            Ok(None)
        }
    }

    /// Getting inactive share balances. There could be multiple inactive pool per staking pool so we have
    /// 2 layers of mapping (table w/ all inactive pools -> staking pool, table w/ delegator inactive shares -> each inactive pool)
    pub async fn get_inactive_share_from_write_table_item(
        write_table_item: &WriteTableItem,
        txn_version: i64,
        write_set_change_index: i64,
        inactive_pool_to_staking_pool: &ShareToStakingPoolMapping,
        inactive_share_to_pool: &ShareToPoolMapping,
        conn: &mut DbPoolConnection<'_>,
        query_retries: u32,
        query_retry_delay_ms: u64,
        block_timestamp: chrono::NaiveDateTime,
    ) -> anyhow::Result<Option<(DelegatorBalance, Self)>> {
        let table_handle = standardize_address(&write_table_item.handle.to_string());
        // The mapping will tell us if the table item belongs to an inactive pool
        if let Some(pool_balance) = inactive_share_to_pool.get(&table_handle) {
            // If it is, we need to get the inactive staking pool handle and use it to look up the staking pool
            let inactive_pool_handle = pool_balance.parent_table_handle.clone();

            let pool_address = match inactive_pool_to_staking_pool
                .get(&inactive_pool_handle)
                .map(|metadata| metadata.staking_pool_address.clone())
            {
                Some(pool_address) => pool_address,
                None => {
                    match Self::get_staking_pool_from_inactive_share_handle(
                        conn,
                        &inactive_pool_handle,
                        query_retries,
                        query_retry_delay_ms,
                    )
                    .await
                    {
                        Ok(pool) => pool,
                        Err(_) => {
                            tracing::error!(
                                transaction_version = txn_version,
                                lookup_key = &inactive_pool_handle,
                                "Failed to get staking pool address from inactive share handle. You probably should backfill db.",
                            );
                            return Ok(None);
                        },
                    }
                },
            };
            let delegator_address = standardize_address(&write_table_item.key.to_string());
            // Convert to TableItem model. Some fields are just placeholders
            let table_item = {
                let (table_item, _) = TableItem::from_write_table_item(
                    write_table_item,
                    0,
                    txn_version,
                    0,
                    block_timestamp,
                );
                PostgresTableItem::from(table_item)
            };

            let shares: BigDecimal = table_item
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
            let shares = shares / &pool_balance.scaling_factor;
            Ok(Some((
                DelegatorBalance {
                    transaction_version: txn_version,
                    write_set_change_index,
                    delegator_address: delegator_address.clone(),
                    pool_address: pool_address.clone(),
                    pool_type: "inactive_shares".to_string(),
                    table_handle: table_handle.clone(),
                    shares: shares.clone(),
                    parent_table_handle: inactive_pool_handle.clone(),
                    block_timestamp,
                },
                Self {
                    delegator_address,
                    pool_address,
                    pool_type: "inactive_shares".to_string(),
                    table_handle: table_handle.clone(),
                    last_transaction_version: txn_version,
                    shares,
                    parent_table_handle: inactive_pool_handle,
                    block_timestamp,
                },
            )))
        } else {
            Ok(None)
        }
    }

    // Setting amount to 0 if table item is deleted
    pub fn get_active_share_from_delete_table_item(
        delete_table_item: &DeleteTableItem,
        txn_version: i64,
        write_set_change_index: i64,
        active_pool_to_staking_pool: &ShareToStakingPoolMapping,
        block_timestamp: chrono::NaiveDateTime,
    ) -> anyhow::Result<Option<(DelegatorBalance, Self)>> {
        let table_handle = standardize_address(&delete_table_item.handle.to_string());
        // The mapping will tell us if the table item is an active share table

        if let Some(pool_balance) = active_pool_to_staking_pool.get(&table_handle) {
            let delegator_address = standardize_address(&delete_table_item.key.to_string());

            return Ok(Some((
                DelegatorBalance {
                    transaction_version: txn_version,
                    write_set_change_index,
                    delegator_address: delegator_address.clone(),
                    pool_address: pool_balance.staking_pool_address.clone(),
                    pool_type: "active_shares".to_string(),
                    table_handle: table_handle.clone(),
                    shares: BigDecimal::zero(),
                    parent_table_handle: table_handle.clone(),
                    block_timestamp,
                },
                Self {
                    delegator_address,
                    pool_address: pool_balance.staking_pool_address.clone(),
                    pool_type: "active_shares".to_string(),
                    table_handle: table_handle.clone(),
                    last_transaction_version: txn_version,
                    shares: BigDecimal::zero(),
                    parent_table_handle: table_handle,
                    block_timestamp,
                },
            )));
        }
        Ok(None)
    }

    // Setting amount to 0 if table item is deleted
    pub async fn get_inactive_share_from_delete_table_item(
        delete_table_item: &DeleteTableItem,
        txn_version: i64,
        write_set_change_index: i64,
        inactive_pool_to_staking_pool: &ShareToStakingPoolMapping,
        inactive_share_to_pool: &ShareToPoolMapping,
        conn: &mut DbPoolConnection<'_>,
        query_retries: u32,
        query_retry_delay_ms: u64,
        block_timestamp: chrono::NaiveDateTime,
    ) -> anyhow::Result<Option<(DelegatorBalance, Self)>> {
        let table_handle = standardize_address(&delete_table_item.handle.to_string());
        // The mapping will tell us if the table item belongs to an inactive pool
        if let Some(pool_balance) = inactive_share_to_pool.get(&table_handle) {
            // If it is, we need to get the inactive staking pool handle and use it to look up the staking pool
            let inactive_pool_handle = pool_balance.parent_table_handle.clone();

            let pool_address = match inactive_pool_to_staking_pool
                .get(&inactive_pool_handle)
                .map(|metadata| metadata.staking_pool_address.clone())
            {
                Some(pool_address) => pool_address,
                None => Self::get_staking_pool_from_inactive_share_handle(
                    conn,
                    &inactive_pool_handle,
                    query_retries,
                    query_retry_delay_ms,
                )
                .await
                .context(format!(
                    "Failed to get staking pool from inactive share handle {inactive_pool_handle}, txn version {txn_version}"
                ))?,
            };
            let delegator_address = standardize_address(&delete_table_item.key.to_string());

            return Ok(Some((
                DelegatorBalance {
                    transaction_version: txn_version,
                    write_set_change_index,
                    delegator_address: delegator_address.clone(),
                    pool_address: pool_address.clone(),
                    pool_type: "inactive_shares".to_string(),
                    table_handle: table_handle.clone(),
                    shares: BigDecimal::zero(),
                    parent_table_handle: inactive_pool_handle.clone(),
                    block_timestamp,
                },
                Self {
                    delegator_address,
                    pool_address,
                    pool_type: "inactive_shares".to_string(),
                    table_handle: table_handle.clone(),
                    last_transaction_version: txn_version,
                    shares: BigDecimal::zero(),
                    parent_table_handle: table_handle,
                    block_timestamp,
                },
            )));
        }
        Ok(None)
    }

    /// Key is the inactive share table handle obtained from 0x1::delegation_pool::DelegationPool
    /// Value is the same metadata although it's not really used
    pub fn get_active_pool_to_staking_pool_mapping(
        write_resource: &WriteResource,
        txn_version: i64,
        block_timestamp: chrono::NaiveDateTime,
    ) -> anyhow::Result<Option<ShareToStakingPoolMapping>> {
        if let Some(balance) = DelegatorPool::get_delegated_pool_metadata_from_write_resource(
            write_resource,
            txn_version,
            block_timestamp,
        )? {
            Ok(Some(AHashMap::from([(
                balance.active_share_table_handle.clone(),
                balance,
            )])))
        } else {
            Ok(None)
        }
    }

    /// Key is the inactive share table handle obtained from 0x1::delegation_pool::DelegationPool
    /// Value is the same metadata although it's not really used
    pub fn get_inactive_pool_to_staking_pool_mapping(
        write_resource: &WriteResource,
        txn_version: i64,
        block_timestamp: chrono::NaiveDateTime,
    ) -> anyhow::Result<Option<ShareToStakingPoolMapping>> {
        if let Some(balance) = DelegatorPool::get_delegated_pool_metadata_from_write_resource(
            write_resource,
            txn_version,
            block_timestamp,
        )? {
            Ok(Some(AHashMap::from([(
                balance.inactive_share_table_handle.clone(),
                balance,
            )])))
        } else {
            Ok(None)
        }
    }

    /// Key is the inactive share table handle obtained from 0x1::pool_u64_unbound::Pool
    /// Value is the 0x1::pool_u64_unbound::Pool metadata that will be used to populate a user's inactive balance
    pub fn get_inactive_share_to_pool_mapping(
        write_table_item: &WriteTableItem,
        txn_version: i64,
    ) -> anyhow::Result<Option<ShareToPoolMapping>> {
        if let Some(balance) = DelegatorPool::get_inactive_pool_metadata_from_write_table_item(
            write_table_item,
            txn_version,
        )? {
            Ok(Some(AHashMap::from([(
                balance.shares_table_handle.clone(),
                balance,
            )])))
        } else {
            Ok(None)
        }
    }

    pub async fn get_staking_pool_from_inactive_share_handle(
        conn: &mut DbPoolConnection<'_>,
        table_handle: &str,
        query_retries: u32,
        query_retry_delay_ms: u64,
    ) -> anyhow::Result<String> {
        let mut tried = 0;
        while tried < query_retries {
            tried += 1;
            match CurrentDelegatorBalanceQuery::get_by_inactive_share_handle(conn, table_handle)
                .await
            {
                Ok(current_delegator_balance) => return Ok(current_delegator_balance.pool_address),
                Err(_) => {
                    if tried < query_retries {
                        tokio::time::sleep(std::time::Duration::from_millis(query_retry_delay_ms))
                            .await;
                    }
                },
            }
        }
        Err(anyhow::anyhow!(
            "Failed to get staking pool address from inactive share handle"
        ))
    }

    pub async fn from_transaction(
        transaction: &Transaction,
        active_pool_to_staking_pool: &ShareToStakingPoolMapping,
        conn: &mut DbPoolConnection<'_>,
        query_retries: u32,
        query_retry_delay_ms: u64,
    ) -> anyhow::Result<(Vec<DelegatorBalance>, CurrentDelegatorBalanceMap)> {
        let mut inactive_pool_to_staking_pool: ShareToStakingPoolMapping = AHashMap::new();
        let mut inactive_share_to_pool: ShareToPoolMapping = AHashMap::new();
        let mut current_delegator_balances: CurrentDelegatorBalanceMap = AHashMap::new();
        let mut delegator_balances = vec![];
        let txn_version = transaction.version as i64;
        let txn_timestamp =
            parse_timestamp(transaction.timestamp.as_ref().unwrap(), txn_version).naive_utc();

        let changes = &transaction.info.as_ref().unwrap().changes;
        // Do a first pass to get the mapping of active_share table handles to staking pool resource        let txn_version = transaction.version as i64;
        for wsc in changes {
            if let Change::WriteResource(write_resource) = wsc.change.as_ref().unwrap() {
                if let Some(map) = Self::get_inactive_pool_to_staking_pool_mapping(
                    write_resource,
                    txn_version,
                    txn_timestamp,
                )
                .unwrap()
                {
                    inactive_pool_to_staking_pool.extend(map);
                }
            }

            if let Change::WriteTableItem(table_item) = wsc.change.as_ref().unwrap() {
                if let Some(map) =
                    Self::get_inactive_share_to_pool_mapping(table_item, txn_version).unwrap()
                {
                    inactive_share_to_pool.extend(map);
                }
            }
        }
        // Now make a pass through table items to get the actual delegator balances
        for (index, wsc) in changes.iter().enumerate() {
            let maybe_delegator_balance = match wsc.change.as_ref().unwrap() {
                Change::DeleteTableItem(table_item) => {
                    if let Some((balance, current_balance)) =
                        Self::get_active_share_from_delete_table_item(
                            table_item,
                            txn_version,
                            index as i64,
                            active_pool_to_staking_pool,
                            txn_timestamp,
                        )
                        .unwrap()
                    {
                        Some((balance, current_balance))
                    } else {
                        Self::get_inactive_share_from_delete_table_item(
                            table_item,
                            txn_version,
                            index as i64,
                            &inactive_pool_to_staking_pool,
                            &inactive_share_to_pool,
                            conn,
                            query_retries,
                            query_retry_delay_ms,
                            txn_timestamp,
                        )
                        .await
                        .unwrap()
                    }
                },
                Change::WriteTableItem(table_item) => {
                    if let Some((balance, current_balance)) =
                        Self::get_active_share_from_write_table_item(
                            table_item,
                            txn_version,
                            index as i64,
                            active_pool_to_staking_pool,
                            txn_timestamp,
                        )
                        .await
                        .unwrap()
                    {
                        Some((balance, current_balance))
                    } else {
                        Self::get_inactive_share_from_write_table_item(
                            table_item,
                            txn_version,
                            index as i64,
                            &inactive_pool_to_staking_pool,
                            &inactive_share_to_pool,
                            conn,
                            query_retries,
                            query_retry_delay_ms,
                            txn_timestamp,
                        )
                        .await
                        .unwrap()
                    }
                },
                _ => None,
            };
            if let Some((delegator_balance, current_delegator_balance)) = maybe_delegator_balance {
                delegator_balances.push(delegator_balance);
                current_delegator_balances.insert(
                    (
                        current_delegator_balance.delegator_address.clone(),
                        current_delegator_balance.pool_address.clone(),
                        current_delegator_balance.pool_type.clone(),
                    ),
                    current_delegator_balance,
                );
            }
        }
        Ok((delegator_balances, current_delegator_balances))
    }
}

impl CurrentDelegatorBalanceQuery {
    pub async fn get_by_inactive_share_handle(
        conn: &mut DbPoolConnection<'_>,
        table_handle: &str,
    ) -> diesel::QueryResult<Self> {
        current_delegator_balances::table
            .filter(current_delegator_balances::parent_table_handle.eq(table_handle))
            .first::<Self>(conn)
            .await
    }
}

// Parquet models
#[derive(
    Allocative, Clone, Debug, Default, Deserialize, FieldCount, ParquetRecordWriter, Serialize,
)]
pub struct ParquetCurrentDelegatorBalance {
    pub delegator_address: String,
    pub pool_address: String,
    pub pool_type: String,
    pub table_handle: String,
    pub last_transaction_version: i64,
    pub shares: String, // BigDecimal
    pub parent_table_handle: String,
    #[allocative(skip)]
    pub block_timestamp: chrono::NaiveDateTime,
}

impl From<CurrentDelegatorBalance> for ParquetCurrentDelegatorBalance {
    fn from(base: CurrentDelegatorBalance) -> Self {
        Self {
            delegator_address: base.delegator_address,
            pool_address: base.pool_address,
            pool_type: base.pool_type,
            table_handle: base.table_handle,
            last_transaction_version: base.last_transaction_version,
            shares: base.shares.to_string(),
            parent_table_handle: base.parent_table_handle,
            block_timestamp: base.block_timestamp,
        }
    }
}

impl HasVersion for ParquetCurrentDelegatorBalance {
    fn version(&self) -> i64 {
        self.last_transaction_version
    }
}

impl NamedTable for ParquetCurrentDelegatorBalance {
    const TABLE_NAME: &'static str = "current_delegator_balances";
}

#[derive(
    Allocative, Clone, Debug, Default, Deserialize, FieldCount, ParquetRecordWriter, Serialize,
)]
pub struct ParquetDelegatorBalance {
    pub transaction_version: i64,
    pub write_set_change_index: i64,
    pub delegator_address: String,
    pub pool_address: String,
    pub pool_type: String,
    pub table_handle: String,
    pub shares: String, // BigDecimal
    pub parent_table_handle: String,
    #[allocative(skip)]
    pub block_timestamp: chrono::NaiveDateTime,
}

impl NamedTable for ParquetDelegatorBalance {
    const TABLE_NAME: &'static str = "delegator_balances";
}

impl HasVersion for ParquetDelegatorBalance {
    fn version(&self) -> i64 {
        self.transaction_version
    }
}

impl From<DelegatorBalance> for ParquetDelegatorBalance {
    fn from(base: DelegatorBalance) -> Self {
        Self {
            transaction_version: base.transaction_version,
            write_set_change_index: base.write_set_change_index,
            delegator_address: base.delegator_address,
            pool_address: base.pool_address,
            pool_type: base.pool_type,
            table_handle: base.table_handle,
            shares: base.shares.to_string(),
            parent_table_handle: base.parent_table_handle,
            block_timestamp: base.block_timestamp,
        }
    }
}

// Postgres models
#[derive(Clone, Debug, Deserialize, FieldCount, Identifiable, Insertable, Serialize)]
#[diesel(primary_key(delegator_address, pool_address, pool_type))]
#[diesel(table_name = current_delegator_balances)]
pub struct PostgresCurrentDelegatorBalance {
    pub delegator_address: String,
    pub pool_address: String,
    pub pool_type: String,
    pub table_handle: String,
    pub last_transaction_version: i64,
    pub shares: BigDecimal,
    pub parent_table_handle: String,
}

impl From<CurrentDelegatorBalance> for PostgresCurrentDelegatorBalance {
    fn from(base: CurrentDelegatorBalance) -> Self {
        Self {
            delegator_address: base.delegator_address,
            pool_address: base.pool_address,
            pool_type: base.pool_type,
            table_handle: base.table_handle,
            last_transaction_version: base.last_transaction_version,
            shares: base.shares,
            parent_table_handle: base.parent_table_handle,
        }
    }
}

#[derive(Clone, Debug, Deserialize, FieldCount, Identifiable, Insertable, Serialize)]
#[diesel(primary_key(transaction_version, write_set_change_index))]
#[diesel(table_name = delegator_balances)]
pub struct PostgresDelegatorBalance {
    pub transaction_version: i64,
    pub write_set_change_index: i64,
    pub delegator_address: String,
    pub pool_address: String,
    pub pool_type: String,
    pub table_handle: String,
    pub shares: BigDecimal,
    pub parent_table_handle: String,
}

impl From<DelegatorBalance> for PostgresDelegatorBalance {
    fn from(base: DelegatorBalance) -> Self {
        Self {
            transaction_version: base.transaction_version,
            write_set_change_index: base.write_set_change_index,
            delegator_address: base.delegator_address,
            pool_address: base.pool_address,
            pool_type: base.pool_type,
            table_handle: base.table_handle,
            shares: base.shares,
            parent_table_handle: base.parent_table_handle,
        }
    }
}
