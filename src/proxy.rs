use reqwest::Client;
use serde::Deserialize;
use std::{collections::HashMap, error::Error};

use crate::{
    sqlite::{SQLite3, Value},
    utils::TursoConfig,
};

#[derive(Debug, Deserialize)]
pub struct RemoteSqliteResponse {
    pub baton: Option<String>,
    pub results: Vec<RemoteSQliteResultType>,
}

#[derive(Debug, Deserialize)]
pub struct RemoteSQliteResultType {
    pub response: RemoteSQLiteResult,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RemoteSQLiteResult {
    Execute { result: QueryResult },
    Close,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RemoteCol {
    pub name: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RemoteRow {
    pub r#type: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Deserialize, Clone)]
pub struct QueryResult {
    pub cols: Vec<RemoteCol>,
    pub rows: Vec<Vec<RemoteRow>>,
    pub last_insert_rowid: Option<String>,
}

pub async fn execute_sql_and_params(
    db: &SQLite3,
    sql: &str,
    params: Vec<serde_json::Value>,
    baton: Option<&String>,
) -> Result<RemoteSqliteResponse, Box<dyn Error>> {
    let mut query_request = serde_json::Map::new();

    if let Some(b) = baton {
        query_request.insert("baton".to_string(), serde_json::json!(b));
    }

    let can_keep_open = !(baton.is_some() && sql.contains("COMMIT"));

    let mut json_array: Vec<serde_json::Value> = Vec::new();

    json_array.push(serde_json::json!({
    "type": "execute",
    "stmt": {
        "sql": sql,
        "args": params
            }
        }));

    if !can_keep_open {
        json_array.push(serde_json::json!({
            "type": "close"
        }));
    }

    query_request.insert("requests".to_string(), json_array.into());

    let result = send_sql_request(
        &db.client,
        &db.turso_config,
        serde_json::Value::from(query_request),
    )
    .await?;

    Ok(result)
}

pub async fn get_transaction_baton(
    client: &Client,
    config: &TursoConfig,
) -> Result<String, Box<dyn Error>> {
    let request = serde_json::json!({
        "requests": [
            {
                "type": "execute",
                "stmt": {
                    "sql": "BEGIN"
                }
            }
        ]
    });

    let result = send_sql_request(client, config, request).await?;
    let baton = result.baton.ok_or("Failed to begin transaction")?;

    Ok(baton)
}

async fn send_sql_request(
    client: &Client,
    config: &TursoConfig,
    request: serde_json::Value,
) -> Result<RemoteSqliteResponse, Box<dyn Error>> {
    if cfg!(debug_assertions) {
        println!("Sending SQL Request: {}", request);
    }

    let response: serde_json::Value =
        send_remote_request(client, config, "v2/pipeline", request).await?;

    let parsed: RemoteSqliteResponse = serde_json::from_value(response)?;
    Ok(parsed)
}

async fn send_remote_request(
    client: &Client,
    turso_config: &TursoConfig,
    path: &str,
    request: serde_json::Value,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let response = client
        .post(format!("https://{}/{}", turso_config.db_url, path))
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", turso_config.db_token))
        .json(&request)
        .send()
        .await?;

    let status = response.status();
    let response_text = response.text().await?;

    if cfg!(debug_assertions) {
        println!("Received Response: {}\n", &response_text);
    }

    if !status.is_success() {
        if let Ok(error_body) = serde_json::from_str::<serde_json::Value>(&response_text) {
            if let Some(error_message) = error_body.get("error").and_then(|e| e.as_str()) {
                return Err(error_message.into());
            }
        }
        return Err(format!("LibSqlite3_Turso Error: {}", response_text).into());
    }

    let parsed_response = serde_json::from_str(&response_text);
    if parsed_response.is_err() {
        return Err(format!("Failed to parse response: {}", parsed_response.unwrap_err()).into());
    }

    let parsed_response: serde_json::Value = parsed_response.unwrap();
    if let Some(results) = parsed_response.get("results").and_then(|r| r.as_array()) {
        for result in results {
            if let Some(error) = result
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
            {
                return Err(error.into());
            }
        }
    }

    Ok(parsed_response)
}

pub fn convert_params_to_json(params: &HashMap<i32, Value>) -> Vec<serde_json::Value> {
    let mut index_value_pairs: Vec<_> = params.iter().collect();
    // Sort by parameter index
    index_value_pairs.sort_by_key(|&(k, _)| *k);

    // Map sorted values to JSON
    index_value_pairs
        .into_iter()
        .map(|(_, value)| match value {
            Value::Integer(i) => serde_json::json!({
                "type": "integer",
                "value": *i.to_string()
            }),

            Value::Real(f) => serde_json::json!({
                "type": "float",
                "value": *f.to_string()
            }),
            Value::Text(s) => serde_json::json!({
                "type": "text",
                "value": s
            }),
            Value::Null => serde_json::json!({
                "type": "null",
                "value": null
            }),
        })
        .collect()
}

pub fn get_execution_result<'a>(
    db: &SQLite3,
    result: &'a RemoteSqliteResponse,
) -> Result<&'a QueryResult, Box<dyn Error>> {
    let mut baton = db.transaction_baton.lock().unwrap();

    if let Some(new_baton) = &result.baton {
        baton.replace(new_baton.into());
    }

    let first_execution_result = match result.results.get(0) {
        Some(inner) => match &inner.response {
            RemoteSQLiteResult::Execute { result } => Ok(result),
            RemoteSQLiteResult::Close => Err::<&'a QueryResult, Box<dyn std::error::Error>>(
                "Unexpected 'close' response".into(),
            ),
        },
        None => Err::<&'a QueryResult, Box<dyn std::error::Error>>("No results returned".into()),
    }?;

    if let Some(last_insert_rowid) = &first_execution_result.last_insert_rowid {
        let mut last_insert_rowid_lock = db.last_insert_rowid.lock().unwrap();
        *last_insert_rowid_lock = Some(last_insert_rowid.parse::<i64>().unwrap_or(0));
    }

    Ok(first_execution_result)
}

pub async fn get_turso_db(client: &Client, db_name: &str) -> Result<TursoConfig, Box<dyn Error>> {
    let globe_auth_api = std::env::var("GLOBE_DS_API")?;

    let request_body = serde_json::json!({
        "db_name": db_name,
    });

    let response = client
        .post(format!("{}/db/auth", globe_auth_api))
        .body(request_body.to_string())
        .send()
        .await
        .map_err(|_| "Failed to fetch auth credentials for database")?;

    if !response.status().is_success() {
        return Err(format!("Failed to get Auth Token: {}", response.status()).into());
    }

    let json = response.json().await?;
    let db_info = serde_json::from_value(json)?;
    Ok(db_info)
}
