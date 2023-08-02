use crate::dynamo::add_collection_activity::{self, CollectionActivity};
use crate::dynamo::add_token::{update_token, UpdateTokenData};
use crate::dynamo::get_collection::get_collection;
use crate::dynamo::update_collection::update_collection;
use crate::dynamo::update_collection_latest_mint::update_collection_latest_mint;
use crate::events::transfer_processor::add_collection_activity::add_collection_activity;
use crate::events::update_token_transfers::update_token_transfers;
use crate::starknet::client::call_contract;
use crate::starknet::utils::TokenId;
use crate::starknet::{client::get_block_with_txs, utils::get_contract_property_string};
use crate::utils::sanitize_uri;
use aws_sdk_dynamodb::types::AttributeValue;
use log::info;
use serde::{Deserialize, Serialize};
use serde_json::to_string;
use serde_json::Value;
use starknet::core::types::{EmittedEvent, FieldElement};
use std::collections::HashMap;
use std::error::Error;

#[derive(Debug, Serialize, Deserialize)]
struct MetadataAttribute {
    trait_type: String,
    value: String,
    display_type: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NormalizedMetadata {
    pub description: String,
    pub external_url: String,
    pub image: String,
    pub name: String,
    attributes: Vec<MetadataAttribute>,
}

impl From<NormalizedMetadata> for HashMap<String, AttributeValue> {
    fn from(metadata: NormalizedMetadata) -> Self {
        let mut attributes: HashMap<String, AttributeValue> = HashMap::new();

        attributes.insert(
            "description".to_string(),
            AttributeValue::S(metadata.description),
        );
        attributes.insert(
            "external_url".to_string(),
            AttributeValue::S(metadata.external_url),
        );
        attributes.insert("image".to_string(), AttributeValue::S(metadata.image));
        attributes.insert("name".to_string(), AttributeValue::S(metadata.name));

        let attributes_list: Vec<AttributeValue> = metadata
            .attributes
            .into_iter()
            .map(|attribute| {
                let mut attribute_map: HashMap<String, AttributeValue> = HashMap::new();
                attribute_map.insert(
                    "trait_type".to_string(),
                    AttributeValue::S(attribute.trait_type),
                );
                attribute_map.insert("value".to_string(), AttributeValue::S(attribute.value));
                attribute_map.insert(
                    "display_type".to_string(),
                    AttributeValue::S(attribute.display_type),
                );
                AttributeValue::M(attribute_map)
            })
            .collect();

        attributes.insert("attributes".to_string(), AttributeValue::L(attributes_list));

        attributes
    }
}
async fn get_token_uri(
    client: &reqwest::Client,
    token_id_low: u128,
    token_id_high: u128,
    contract_address: &str,
    block_number: u64,
) -> String {
    info!("get_token_id: [{:?}, {:?}]", token_id_low, token_id_high);

    let token_id_low_hex = format!("{:x}", token_id_low);
    let token_id_high_hex = format!("{:x}", token_id_high);

    let token_uri_cairo0 = get_contract_property_string(
        client,
        contract_address,
        "tokenURI",
        vec![&token_id_low_hex, &token_id_high_hex],
        block_number,
    )
    .await;

    if token_uri_cairo0 != "undefined" && !token_uri_cairo0.is_empty() {
        return token_uri_cairo0;
    }

    let token_uri = get_contract_property_string(
        client,
        contract_address,
        "token_uri",
        vec![&token_id_low_hex, &token_id_high_hex],
        block_number,
    )
    .await;

    info!("token_uri: {:?}", token_uri);

    if token_uri != "undefined" && !token_uri.is_empty() {
        return token_uri;
    }

    "undefined".to_string()
}

async fn update_additional_collection_data(
    client: &reqwest::Client,
    dynamo_client: &aws_sdk_dynamodb::Client,
    contract_address: &str,
    contract_type: String,
    block_number: u64,
) -> Result<(), Box<dyn Error>> {
    info!("update_additional_collection_data");

    let collection_symbol =
        get_contract_property_string(client, contract_address, "symbol", vec![], block_number)
            .await;

    let collection_name =
        get_contract_property_string(client, contract_address, "name", vec![], block_number).await;

    info!("collection_name: {:?}", collection_name);

    update_collection(
        dynamo_client,
        contract_address.to_string(),
        contract_type.to_string(),
        collection_name,
        collection_symbol,
    )
    .await?;

    Ok(())
}

pub async fn process_transfers(
    client: &reqwest::Client,
    dynamo_db_client: &aws_sdk_dynamodb::Client,
    value: &str,
    contract_type: &str,
) -> Result<(), Box<dyn Error>> {
    println!("Processing transfers: {:?}", value);

    //let data = str::from_utf8(&value.as_bytes())?;
    let event: EmittedEvent = serde_json::from_str(value)?;

    // Get block info
    let block = get_block_with_txs(client, event.block_number)
        .await
        .unwrap();
    let timestamp = block.get("timestamp").unwrap().as_u64().unwrap();

    // Extracting "data" from event
    let from_address = format!("{:#064x}", event.data[0]);
    let to_address = format!("{:#064x}", event.data[1]);
    let contract_address = format!("{:#064x}", event.from_address);
    let transaction_hash = format!("{:#064x}", event.transaction_hash);

    let token_id_low = event.data[2];
    let token_id_high = event.data[3];

    let token_id = TokenId {
        low: token_id_low,
        high: token_id_high,
    };

    let formated_token_id = token_id.format();

    let block_number = event.block_number;
    let token_uri = get_token_uri(
        client,
        formated_token_id.low,
        formated_token_id.high,
        &contract_address,
        block_number,
    )
    .await;

    let token_owner = get_token_owner(
        client,
        token_id_low,
        token_id_high,
        contract_address.as_str(),
        block_number,
    )
    .await;

    info!(
        "Contract address: {} - Token ID: {} - Token URI: {} - Block number: {}",
        contract_address, formated_token_id.token_id, token_uri, block_number
    );

    update_additional_collection_data(
        client,
        dynamo_db_client,
        contract_address.as_str(),
        contract_type.to_string(),
        block_number,
    )
    .await
    .unwrap();

    let _transfer = update_token_transfers(
        dynamo_db_client,
        &contract_address,
        formated_token_id.padded_token_id.clone(),
        &from_address,
        &to_address,
        &timestamp,
        &transaction_hash,
    )
    .await;

    if event.data[0] == FieldElement::ZERO {
        info!(
        "\n\n=== MINT DETECTED ===\n\nContract address: {} - Token ID: {} - Token URI: {} - Block number: {}\n\n===========\n\n",
        contract_address, formated_token_id.token_id, token_uri, block_number
    );

        let transaction_data = TransactionData {
            timestamp,
            block_number,
            from_address: from_address.to_string(),
            to_address: to_address.to_string(),
            hash: transaction_hash.to_string(),
        };

        process_mint_event(
            client,
            dynamo_db_client,
            contract_address.as_str(),
            TokenData {
                padded_token_id: formated_token_id.padded_token_id.clone(),
                token_uri,
                owner: token_owner,
                token_type: contract_type.to_string(),
            },
            transaction_data,
        )
        .await;
    } else {
        // TODO
    }

    Ok(())
}

async fn get_token_owner(
    client: &reqwest::Client,
    token_id_low: FieldElement,
    token_id_high: FieldElement,
    contract_address: &str,
    block_number: u64,
) -> String {
    let token_id_low_hex = format!("{:x}", token_id_low);
    let token_id_high_hex = format!("{:x}", token_id_high);
    let calldata = vec![token_id_low_hex.as_str(), token_id_high_hex.as_str()];

    match call_contract(client, contract_address, "ownerOf", calldata, block_number).await {
        Ok(result) => {
            if let Some(token_owner) = result.get(0) {
                token_owner.to_string().replace('\"', "")
            } else {
                "".to_string()
            }
        }
        Err(_error) => "".to_string(),
    }
}

pub struct TokenData {
    pub padded_token_id: String,
    pub token_uri: String,
    pub owner: String,
    pub token_type: String,
}

pub struct TransactionData {
    pub timestamp: u64,
    pub block_number: u64,
    pub from_address: String,
    pub to_address: String,
    pub hash: String,
}

async fn process_mint_event(
    client: &reqwest::Client,
    dynamo_client: &aws_sdk_dynamodb::Client,
    collection_address: &str,
    token_data: TokenData,
    transaction_data: TransactionData,
) {
    let (metadata_uri, initial_metadata_uri) = sanitize_uri(token_data.token_uri.as_str()).await;

    info!(
        "metadata_uri: {:?} - initial_metadata_uri: {:?} - token_uri: {:?}",
        metadata_uri, initial_metadata_uri, token_data.token_uri
    );

    let collection_result = get_collection(dynamo_client, collection_address.to_string()).await;
    info!(
        "collection_result: {:?} - with collection address: {:?}",
        collection_result, collection_address
    );

    match collection_result {
        Ok(Some(collection)) => {
            println!("collection: {:?}", collection);

            if let Some(latest_mint) = collection.get("latest_mint") {
                let latest_mint_str = latest_mint.as_s().unwrap();
                match latest_mint_str.parse::<u64>() {
                    Ok(latest_mint_value) => {
                        println!(
                            "Check latest mint: {:?} / {:?}",
                            latest_mint_value, transaction_data.timestamp
                        );

                        if latest_mint_value > transaction_data.timestamp {
                            let _ = update_collection_latest_mint(
                                dynamo_client,
                                latest_mint_value,
                                collection_address.to_string(),
                                token_data.token_type.to_string(),
                            )
                            .await;
                        }
                    }
                    Err(parse_err) => {
                        info!("Error parsing latest_mint: {}", parse_err);
                    }
                }
            } else {
                let _ = update_collection_latest_mint(
                    dynamo_client,
                    transaction_data.timestamp,
                    collection_address.to_string(),
                    token_data.token_type.to_string(),
                )
                .await;
            }

            //  TODO: Inserting into ark_mainnet_collection_activities

            let activity = CollectionActivity {
                address: collection_address.to_string(),
                timestamp: transaction_data.timestamp,
                block_number: transaction_data.block_number,
                event_type: "mint".to_string(),
                from_address: transaction_data.from_address.clone(),
                padded_token_id: token_data.padded_token_id.clone(),
                token_uri: token_data.token_uri.clone(),
                to_address: transaction_data.to_address.clone(),
                transaction_hash: transaction_data.hash.clone(),
                token_type: token_data.token_type.clone(),
            };

            let _ = add_collection_activity(dynamo_client, activity).await;
        }
        Ok(None) => {
            info!("No collection found at address");
        }
        Err(err) => {
            info!("Error getting collection: {}", err);
        }
    }

    println!("metadata_uri: {:?}", metadata_uri);

    if !metadata_uri.is_empty() {
        let result =
            fetch_metadata(client, metadata_uri.as_str(), initial_metadata_uri.as_str()).await;

        match result {
            Ok((raw_metadata, normalized_metadata)) => {
                println!(
                    "Raw metadata: {:?} - Normalized_metadata: {:?}",
                    raw_metadata, normalized_metadata
                );

                // TODO: Uploading image to S3

                let update_token_data = UpdateTokenData {
                    collection_address: collection_address.to_string(),
                    padded_token_id: token_data.padded_token_id.clone(),
                    token_uri: token_data.token_uri,
                    owner: token_data.owner,
                    mint_transaction_hash: transaction_data.hash,
                    block_number_minted: transaction_data.block_number,
                    raw: to_string(&raw_metadata).unwrap(),
                    normalized: normalized_metadata,
                };

                let _ = update_token(dynamo_client, update_token_data).await;
            }
            Err(e) => {
                info!("Error fetching metadata: {}", e);
            }
        };
    }
}

pub async fn fetch_metadata(
    client: &reqwest::Client,
    metadata_uri: &str,
    initial_metadata_uri: &str,
) -> Result<(Value, NormalizedMetadata), Box<dyn Error>> {
    println!("Fetching metadata: {}", metadata_uri);

    let response = client.get(metadata_uri).send().await?;
    let raw_metadata: Value = response.json().await?;

    println!("Metadata: {:?}", raw_metadata);

    let empty_vec = Vec::new();

    let attributes = raw_metadata
        .get("attributes")
        .and_then(|attr| attr.as_array())
        .unwrap_or(&empty_vec);

    let normalized_attributes: Vec<MetadataAttribute> = attributes
        .iter()
        .map(|attribute| MetadataAttribute {
            trait_type: attribute
                .get("trait_type")
                .and_then(|trait_type| trait_type.as_str())
                .unwrap_or("")
                .to_string(),
            value: attribute
                .get("value")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string(),
            display_type: attribute
                .get("display_type")
                .and_then(|display_type| display_type.as_str())
                .unwrap_or("")
                .to_string(),
        })
        .collect();

    let normalized_metadata = NormalizedMetadata {
        description: raw_metadata
            .get("description")
            .and_then(|desc| desc.as_str())
            .unwrap_or("")
            .to_string(),
        external_url: initial_metadata_uri.to_string(),
        image: raw_metadata
            .get("image")
            .and_then(|img| img.as_str())
            .unwrap_or("")
            .to_string(),
        name: raw_metadata
            .get("name")
            .and_then(|name| name.as_str())
            .unwrap_or("")
            .to_string(),
        attributes: normalized_attributes,
    };

    Ok((raw_metadata, normalized_metadata))
}