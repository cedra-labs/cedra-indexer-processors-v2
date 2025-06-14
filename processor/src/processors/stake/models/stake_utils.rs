// Copyright © Cedra Foundation
// SPDX-License-Identifier: Apache-2.0

use crate::processors::{
    default::models::move_resources::MoveResource, token_v2::token_models::token_utils::Table,
};
use anyhow::{Context, Result};
use cedra_indexer_processor_sdk::{
    cedra_protos::transaction::v1::WriteResource,
    utils::convert::{deserialize_from_string, standardize_address},
};
use bigdecimal::BigDecimal;
use serde::{Deserialize, Serialize};
use tracing::error;

const STAKE_ADDR: &str = "0x0000000000000000000000000000000000000000000000000000000000000001";

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StakePoolResource {
    delegated_voter: String,
    operator_address: String,
}

impl StakePoolResource {
    pub fn get_delegated_voter(&self) -> String {
        standardize_address(&self.delegated_voter)
    }

    pub fn get_operator_address(&self) -> String {
        standardize_address(&self.operator_address)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DelegationPoolResource {
    pub active_shares: PoolResource,
    pub inactive_shares: Table,
    #[serde(deserialize_with = "deserialize_from_string")]
    pub operator_commission_percentage: BigDecimal,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PoolResource {
    pub shares: SharesInnerResource,
    #[serde(deserialize_with = "deserialize_from_string")]
    pub total_coins: BigDecimal,
    #[serde(deserialize_with = "deserialize_from_string")]
    pub total_shares: BigDecimal,
    #[serde(deserialize_with = "deserialize_from_string")]
    pub scaling_factor: BigDecimal,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SharesInnerResource {
    pub inner: Table,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GovernanceVoteEvent {
    #[serde(deserialize_with = "deserialize_from_string")]
    pub proposal_id: u64,
    pub voter: String,
    pub stake_pool: String,
    #[serde(deserialize_with = "deserialize_from_string")]
    pub num_votes: BigDecimal,
    pub should_pass: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DistributeRewardsEvent {
    pub pool_address: String,
    #[serde(deserialize_with = "deserialize_from_string")]
    pub rewards_amount: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AddStakeEvent {
    #[serde(deserialize_with = "deserialize_from_string")]
    pub amount_added: u64,
    pub delegator_address: String,
    pub pool_address: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UnlockStakeEvent {
    #[serde(deserialize_with = "deserialize_from_string")]
    pub amount_unlocked: u64,
    pub delegator_address: String,
    pub pool_address: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WithdrawStakeEvent {
    #[serde(deserialize_with = "deserialize_from_string")]
    pub amount_withdrawn: u64,
    pub delegator_address: String,
    pub pool_address: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ReactivateStakeEvent {
    #[serde(deserialize_with = "deserialize_from_string")]
    pub amount_reactivated: u64,
    pub delegator_address: String,
    pub pool_address: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum StakeTableItem {
    Pool(PoolResource),
}

impl StakeTableItem {
    pub fn from_table_item_type(
        data_type: &str,
        data: &str,
        txn_version: i64,
    ) -> Result<Option<Self>> {
        match data_type {
            "0x1::pool_u64_unbound::Pool" => {
                serde_json::from_str(data).map(|inner| Some(StakeTableItem::Pool(inner)))
            },
            _ => Ok(None),
        }
        .context(format!(
            "version {txn_version} failed! failed to parse type {data_type}, data {data:?}"
        ))
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum StakeResource {
    StakePool(StakePoolResource),
    DelegationPool(DelegationPoolResource),
}

impl StakeResource {
    fn is_resource_supported(data_type: &str) -> bool {
        [
            format!("{STAKE_ADDR}::stake::StakePool"),
            format!("{STAKE_ADDR}::delegation_pool::DelegationPool"),
        ]
        .contains(&data_type.to_string())
    }

    fn from_resource(data_type: &str, data: &serde_json::Value, txn_version: i64) -> Result<Self> {
        match data_type {
            x if x == format!("{STAKE_ADDR}::stake::StakePool") => {
                serde_json::from_value(data.clone())
                    .map(|inner| Some(StakeResource::StakePool(inner)))
            },
            x if x == format!("{STAKE_ADDR}::delegation_pool::DelegationPool") => {
                serde_json::from_value(data.clone())
                    .map(|inner| Some(StakeResource::DelegationPool(inner)))
            },
            _ => Ok(None),
        }
        .context(format!(
            "version {txn_version} failed! failed to parse type {data_type}, data {data:?}"
        ))?
        .context(format!(
            "Resource unsupported! Call is_resource_supported first. version {txn_version} type {data_type}"
        ))
    }

    pub fn from_write_resource(
        write_resource: &WriteResource,
        txn_version: i64,
        block_timestamp: chrono::NaiveDateTime,
    ) -> Result<Option<Self>> {
        let type_str = MoveResource::get_outer_type_from_write_resource(write_resource);
        if !Self::is_resource_supported(type_str.as_str()) {
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
                error!(
                    "Error processing write resource for transaction version {}: {}",
                    txn_version, e
                );
                return Err(e);
            },
        };
        Ok(Some(Self::from_resource(
            &type_str,
            resource.data.as_ref().unwrap(),
            txn_version,
        )?))
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum StakeEvent {
    GovernanceVoteEvent(GovernanceVoteEvent),
    DistributeRewardsEvent(DistributeRewardsEvent),
    AddStakeEvent(AddStakeEvent),
    UnlockStakeEvent(UnlockStakeEvent),
    WithdrawStakeEvent(WithdrawStakeEvent),
    ReactivateStakeEvent(ReactivateStakeEvent),
}

impl StakeEvent {
    pub fn from_event(data_type: &str, data: &str, txn_version: i64) -> Result<Option<Self>> {
        match data_type {
            "0x1::cedra_governance::VoteEvent" | "0x1::cedra_governance::Vote" => {
                serde_json::from_str(data).map(|inner| Some(StakeEvent::GovernanceVoteEvent(inner)))
            },
            "0x1::stake::DistributeRewardsEvent" | "0x1::stake::DistributeRewards" => {
                serde_json::from_str(data)
                    .map(|inner| Some(StakeEvent::DistributeRewardsEvent(inner)))
            },
            "0x1::delegation_pool::AddStakeEvent" | "0x1::delegation_pool::AddStake" => {
                serde_json::from_str(data).map(|inner| Some(StakeEvent::AddStakeEvent(inner)))
            },
            "0x1::delegation_pool::UnlockStakeEvent" | "0x1::delegation_pool::UnlockStake" => {
                serde_json::from_str(data).map(|inner| Some(StakeEvent::UnlockStakeEvent(inner)))
            },
            "0x1::delegation_pool::WithdrawStakeEvent" | "0x1::delegation_pool::WithdrawStake" => {
                serde_json::from_str(data).map(|inner| Some(StakeEvent::WithdrawStakeEvent(inner)))
            },
            "0x1::delegation_pool::ReactivateStakeEvent"
            | "0x1::delegation_pool::ReactivateStake" => serde_json::from_str(data)
                .map(|inner| Some(StakeEvent::ReactivateStakeEvent(inner))),
            _ => Ok(None),
        }
        .context(format!(
            "version {txn_version} failed! failed to parse type {data_type}, data {data:?}"
        ))
    }
}

pub enum VoteDelegationTableItem {
    VoteDelegationVector(Vec<VoteDelegationVector>),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VoteDelegationVector {
    key: String,
    pub value: VoteDelegationResource,
}

impl VoteDelegationVector {
    pub fn get_delegator_address(&self) -> String {
        standardize_address(&self.key)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VoteDelegationResource {
    pub voter: String,
    pub pending_voter: String,
}

impl VoteDelegationResource {
    pub fn get_voter(&self) -> String {
        standardize_address(&self.voter)
    }

    pub fn get_pending_voter(&self) -> String {
        standardize_address(&self.pending_voter)
    }
}

impl VoteDelegationTableItem {
    pub fn from_table_item_type(
        data_type: &str,
        data: &str,
        txn_version: i64,
    ) -> Result<Option<Self>> {
        match data_type {
            "vector<0x1::smart_table::Entry<address, 0x1::delegation_pool::VoteDelegation>>" => {
                let vote_delegation_vector: Vec<VoteDelegationVector> = serde_json::from_str(data)
                    .context(format!(
                        "version {txn_version} failed! failed to parse type {data_type}, data {data:?}"
                    ))?;
                Ok(Some(VoteDelegationTableItem::VoteDelegationVector(
                    vote_delegation_vector.clone(),
                )))
            },
            _ => Ok(None),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GovernanceRecordsResource {
    pub vote_delegation: VoteDelegationBucketsResource,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VoteDelegationBucketsResource {
    pub buckets: VoteDelegationInnerResource,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VoteDelegationInnerResource {
    pub inner: Table,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DelegationVoteGovernanceRecordsResource {
    GovernanceRecords(GovernanceRecordsResource),
}

impl DelegationVoteGovernanceRecordsResource {
    pub fn from_resource(
        data_type: &str,
        data: &serde_json::Value,
        txn_version: i64,
    ) -> Result<Option<Self>> {
        match data_type {
            x if x == format!("{STAKE_ADDR}::delegation_pool::GovernanceRecords") => {
                serde_json::from_value(data.clone()).map(|inner| {
                    Some(DelegationVoteGovernanceRecordsResource::GovernanceRecords(
                        inner,
                    ))
                })
            },
            _ => Ok(None),
        }
        .context(format!(
            "version {txn_version} failed! failed to parse type {data_type}, data {data:?}"
        ))
    }

    pub fn from_write_resource(
        write_resource: &WriteResource,
        txn_version: i64,
        block_timestamp: chrono::NaiveDateTime,
    ) -> Result<Option<Self>> {
        let type_str = MoveResource::get_outer_type_from_write_resource(write_resource);
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
                error!(
                    "Error processing write resource for transaction version {}: {}",
                    txn_version, e
                );
                return Err(e);
            },
        };
        Self::from_resource(&type_str, resource.data.as_ref().unwrap(), txn_version)
    }
}
