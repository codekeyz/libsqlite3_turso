use reqwest::Client;
use serde::Deserialize;
use std::{collections::HashMap, error::Error, time::Duration};

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

    let mut json_array: Vec<serde_json::Value> = Vec::new();

    json_array.push(serde_json::json!({
    "type": "execute",
    "stmt": {
        "sql": sql,
        "args": params
            }
        }));

    if db.has_began_transaction() {
        query_request.insert("baton".to_string(), serde_json::json!(baton));
    } else {
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

pub async fn send_remote_request(
    client: &Client,
    turso_config: &TursoConfig,
    path: &str,
    request: serde_json::Value,
) -> Result<serde_json::Value, Box<dyn Error>> {
    const MAX_ATTEMPTS: usize = 5;
    let mut last_error = String::new();

    for attempt in 1..=MAX_ATTEMPTS {
        if cfg!(debug_assertions) {
            println!(
                "Attempt {}: Sending request to {}",
                attempt, turso_config.db_url
            );
        }

        let resp = client
            .post(format!("https://{}/{}", turso_config.db_url, path))
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", turso_config.db_token))
            .json(&request)
            .send()
            .await;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                last_error = format!("Request failed: {}", e);
                if attempt < MAX_ATTEMPTS {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                } else {
                    return Err(last_error.into());
                }
            }
        };

        let status = resp.status();
        let text = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                last_error = format!("Failed to read response body: {}", e);
                if attempt < MAX_ATTEMPTS {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                } else {
                    return Err(last_error.into());
                }
            }
        };

        if cfg!(debug_assertions) {
            println!("Response received: {}", text);
        }

        if !status.is_success() {
            if let Ok(err_json) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(msg) = err_json.get("error").and_then(|v| v.as_str()) {
                    last_error = format!("API error: {}", msg);
                } else {
                    last_error = format!("HTTP error {}: {}", status, text);
                }
            } else {
                last_error = format!("HTTP error {} with invalid JSON: {}", status, text);
            }

            if attempt < MAX_ATTEMPTS {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            } else {
                return Err(last_error.into());
            }
        }

        let parsed: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => return Err(format!("Failed to parse JSON: {}", e).into()),
        };

        // Check for embedded DB errors
        if let Some(results) = parsed.get("results").and_then(|r| r.as_array()) {
            for result in results {
                if let Some(msg) = result
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                {
                    return Err(msg.to_string().into());
                }
            }
        }

        return Ok(parsed);
    }

    Err(format!(
        "Failed to get successful response after {} attempts: {}",
        MAX_ATTEMPTS, last_error
    )
    .into())
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
