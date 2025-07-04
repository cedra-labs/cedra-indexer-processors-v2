use crate::{
    parquet_processors::{
        parquet_utils::util::add_to_map_if_opted_in_for_backfill, ParquetTypeEnum,
        ParquetTypeStructs,
    },
    processors::token_v2::{
        token_models::{
            token_claims::ParquetCurrentTokenPendingClaim,
            token_royalty::ParquetCurrentTokenRoyaltyV1, tokens::TableMetadataForToken,
        },
        token_v2_models::{
            v2_collections::ParquetCollectionV2,
            v2_token_activities::ParquetTokenActivityV2,
            v2_token_datas::{ParquetCurrentTokenDataV2, ParquetTokenDataV2},
            v2_token_metadata::ParquetCurrentTokenV2Metadata,
            v2_token_ownerships::{ParquetCurrentTokenOwnershipV2, ParquetTokenOwnershipV2},
        },
        token_v2_processor_helpers::parse_v2_token,
    },
    utils::table_flags::TableFlags,
};
use cedra_indexer_processor_sdk::{
    cedra_protos::transaction::v1::Transaction,
    traits::{async_step::AsyncRunType, AsyncStep, NamedStep, Processable},
    types::transaction_context::TransactionContext,
    utils::errors::ProcessorError,
};
use async_trait::async_trait;
use std::collections::HashMap;

/// Extracts parquet data from transactions, allowing optional selection of specific tables.
pub struct ParquetTokenV2Extractor
where
    Self: Processable + Send + Sized + 'static,
{
    pub opt_in_tables: TableFlags,
}

type ParquetTypeMap = HashMap<ParquetTypeEnum, ParquetTypeStructs>;

#[async_trait]
impl Processable for ParquetTokenV2Extractor {
    type Input = Vec<Transaction>;
    type Output = ParquetTypeMap;
    type RunType = AsyncRunType;

    async fn process(
        &mut self,
        transactions: TransactionContext<Self::Input>,
    ) -> anyhow::Result<Option<TransactionContext<ParquetTypeMap>>, ProcessorError> {
        // First get all token related table metadata from the batch of transactions. This is in case
        // an earlier transaction has metadata (in resources) that's missing from a later transaction.
        let table_handle_to_owner: ahash::AHashMap<String, TableMetadataForToken> =
            TableMetadataForToken::get_table_handle_to_owner_from_transactions(&transactions.data);

        let (
            collections_v2,
            raw_token_datas_v2,
            raw_token_ownerships_v2,
            _current_collections_v2,
            raw_current_token_datas_v2,
            raw_current_deleted_token_datas_v2,
            raw_current_token_ownerships_v2,
            raw_current_deleted_token_ownerships_v2,
            raw_token_activities_v2,
            raw_current_token_v2_metadata,
            raw_current_token_royalties_v1,
            raw_current_token_claims,
        ) = parse_v2_token(&transactions.data, &table_handle_to_owner, &mut None).await;

        let parquet_current_token_claims: Vec<ParquetCurrentTokenPendingClaim> =
            raw_current_token_claims
                .into_iter()
                .map(ParquetCurrentTokenPendingClaim::from)
                .collect();

        let parquet_current_token_royalties_v1: Vec<ParquetCurrentTokenRoyaltyV1> =
            raw_current_token_royalties_v1
                .into_iter()
                .map(ParquetCurrentTokenRoyaltyV1::from)
                .collect();

        let parquet_current_token_v2_metadata: Vec<ParquetCurrentTokenV2Metadata> =
            raw_current_token_v2_metadata
                .into_iter()
                .map(ParquetCurrentTokenV2Metadata::from)
                .collect();

        let parquet_token_activities_v2: Vec<ParquetTokenActivityV2> = raw_token_activities_v2
            .into_iter()
            .map(ParquetTokenActivityV2::from)
            .collect();

        let parquet_token_datas_v2: Vec<ParquetTokenDataV2> = raw_token_datas_v2
            .into_iter()
            .map(ParquetTokenDataV2::from)
            .collect();

        let parquet_current_token_datas_v2: Vec<ParquetCurrentTokenDataV2> =
            raw_current_token_datas_v2
                .into_iter()
                .map(ParquetCurrentTokenDataV2::from)
                .collect();

        let parquet_deleted_current_token_datss_v2: Vec<ParquetCurrentTokenDataV2> =
            raw_current_deleted_token_datas_v2
                .into_iter()
                .map(ParquetCurrentTokenDataV2::from)
                .collect();

        let parquet_token_ownerships_v2: Vec<ParquetTokenOwnershipV2> = raw_token_ownerships_v2
            .into_iter()
            .map(ParquetTokenOwnershipV2::from)
            .collect();

        let parquet_current_token_ownerships_v2: Vec<ParquetCurrentTokenOwnershipV2> =
            raw_current_token_ownerships_v2
                .into_iter()
                .map(ParquetCurrentTokenOwnershipV2::from)
                .collect();

        let parquet_deleted_current_token_ownerships_v2: Vec<ParquetCurrentTokenOwnershipV2> =
            raw_current_deleted_token_ownerships_v2
                .into_iter()
                .map(ParquetCurrentTokenOwnershipV2::from)
                .collect();

        let parquet_collections_v2: Vec<ParquetCollectionV2> = collections_v2
            .into_iter()
            .map(ParquetCollectionV2::from)
            .collect();

        // We are merging these two tables, b/c they are essentially the same table
        let mut combined_current_token_datas_v2: Vec<ParquetCurrentTokenDataV2> = Vec::new();
        parquet_current_token_datas_v2
            .iter()
            .for_each(|x| combined_current_token_datas_v2.push(x.clone()));
        parquet_deleted_current_token_datss_v2
            .iter()
            .for_each(|x| combined_current_token_datas_v2.push(x.clone()));

        let mut merged_current_token_ownerships_v2: Vec<ParquetCurrentTokenOwnershipV2> =
            Vec::new();
        parquet_current_token_ownerships_v2
            .iter()
            .for_each(|x| merged_current_token_ownerships_v2.push(x.clone()));
        parquet_deleted_current_token_ownerships_v2
            .iter()
            .for_each(|x| merged_current_token_ownerships_v2.push(x.clone()));

        let mut map: HashMap<ParquetTypeEnum, ParquetTypeStructs> = HashMap::new();

        // Array of tuples for each data type and its corresponding enum variant and flag
        let data_types = [
            (
                TableFlags::CURRENT_TOKEN_PENDING_CLAIMS,
                ParquetTypeEnum::CurrentTokenPendingClaims,
                ParquetTypeStructs::CurrentTokenPendingClaim(parquet_current_token_claims),
            ),
            (
                TableFlags::CURRENT_TOKEN_ROYALTY_V1,
                ParquetTypeEnum::CurrentTokenRoyaltiesV1,
                ParquetTypeStructs::CurrentTokenRoyaltyV1(parquet_current_token_royalties_v1),
            ),
            (
                TableFlags::CURRENT_TOKEN_V2_METADATA,
                ParquetTypeEnum::CurrentTokenV2Metadata,
                ParquetTypeStructs::CurrentTokenV2Metadata(parquet_current_token_v2_metadata),
            ),
            (
                TableFlags::TOKEN_ACTIVITIES_V2,
                ParquetTypeEnum::TokenActivitiesV2,
                ParquetTypeStructs::TokenActivityV2(parquet_token_activities_v2),
            ),
            (
                TableFlags::TOKEN_DATAS_V2,
                ParquetTypeEnum::TokenDatasV2,
                ParquetTypeStructs::TokenDataV2(parquet_token_datas_v2),
            ),
            (
                TableFlags::CURRENT_TOKEN_DATAS_V2,
                ParquetTypeEnum::CurrentTokenDatasV2,
                ParquetTypeStructs::CurrentTokenDataV2(combined_current_token_datas_v2),
            ),
            (
                TableFlags::TOKEN_OWNERSHIPS_V2,
                ParquetTypeEnum::TokenOwnershipsV2,
                ParquetTypeStructs::TokenOwnershipV2(parquet_token_ownerships_v2),
            ),
            (
                TableFlags::CURRENT_TOKEN_OWNERSHIPS_V2,
                ParquetTypeEnum::CurrentTokenOwnershipsV2,
                ParquetTypeStructs::CurrentTokenOwnershipV2(merged_current_token_ownerships_v2),
            ),
            (
                TableFlags::COLLECTIONS_V2,
                ParquetTypeEnum::CollectionsV2,
                ParquetTypeStructs::CollectionV2(parquet_collections_v2),
            ),
        ];

        // Populate the map based on opt-in tables
        add_to_map_if_opted_in_for_backfill(self.opt_in_tables, &mut map, data_types.to_vec());

        Ok(Some(TransactionContext {
            data: map,
            metadata: transactions.metadata,
        }))
    }
}

impl AsyncStep for ParquetTokenV2Extractor {}

impl NamedStep for ParquetTokenV2Extractor {
    fn name(&self) -> String {
        "ParquetTokenV2Extractor".to_string()
    }
}
