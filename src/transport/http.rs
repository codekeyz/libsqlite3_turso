use std::{sync::Arc, time::Duration};

use crate::{
    sqlite::{SqliteError, SQLITE_ERROR},
    transport::{LibsqlInterface, RemoteSqliteResponse, TursoConfig},
};

pub struct HttpStrategy {
    client: reqwest::Client,
    turso_config: Arc<TursoConfig>,
}

impl HttpStrategy {
    pub fn new(client: reqwest::Client, turso_config: Arc<TursoConfig>) -> Self {
        Self {
            client,
            turso_config,
        }
    }
}

impl LibsqlInterface for HttpStrategy {
    async fn get_transaction_baton(&mut self, sql: &str) -> Result<String, SqliteError> {
        let request = serde_json::json!({
            "requests": [
                {
                    "type": "execute",
                    "stmt": {
                        "sql": sql
                    }
                }
            ]
        });
        let result = self.send(request).await;
        if let Err(e) = result {
            return Err(SqliteError::new(
                format!("Failed to get transaction baton: {}", e),
                Some(SQLITE_ERROR),
            ));
        }
        let result = result.unwrap();
        let baton = result.baton.ok_or(SqliteError::new(
            "Failed to get transaction baton",
            Some(SQLITE_ERROR),
        ))?;

        Ok(baton)
    }

    async fn send(
        &mut self,
        request: serde_json::Value,
    ) -> Result<RemoteSqliteResponse, SqliteError> {
        const MAX_ATTEMPTS: usize = 5;
        let mut last_error = String::new();

        for attempt in 1..=MAX_ATTEMPTS {
            if cfg!(debug_assertions) {
                println!(
                    "Attempt {}: Sending request to {}",
                    attempt, self.turso_config.db_url
                );
            }

            let resp = self
                .client
                .post(format!("https://{}/v2/pipeline", self.turso_config.db_url))
                .header("Content-Type", "application/json")
                .header(
                    "Authorization",
                    format!("Bearer {}", self.turso_config.db_token),
                )
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
                        return Err(SqliteError::new(last_error, Some(SQLITE_ERROR)));
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
                        return Err(SqliteError::new(last_error, Some(SQLITE_ERROR)));
                    }
                }
            };

            if cfg!(debug_assertions) {
                println!("Response received, status: {} : {}", status, text);
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
                    return Err(SqliteError::new(last_error, Some(SQLITE_ERROR)));
                }
            }

            let parsed: serde_json::Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    return Err(SqliteError::new(
                        format!("Failed to parse JSON: {}", e),
                        Some(SQLITE_ERROR),
                    ))
                }
            };

            // Check for embedded DB errors
            if let Some(results) = parsed.get("results").and_then(|r| r.as_array()) {
                for result in results {
                    if let Some(msg) = result
                        .get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                    {
                        return Err(SqliteError::new(msg.to_string(), Some(SQLITE_ERROR)));
                    }
                }
            }

            let parsed: RemoteSqliteResponse = serde_json::from_value(parsed).map_err(|e| {
                SqliteError::new(
                    format!("Failed to parse response: {}", e),
                    Some(SQLITE_ERROR),
                )
            })?;
            return Ok(parsed);
        }

        Err(SqliteError::new(last_error, Some(SQLITE_ERROR)))
    }

    fn get_json_request(
        &self,
        sql: &str,
        params: Vec<serde_json::Value>,
        baton: Option<&String>,
        is_transacting: bool,
    ) -> serde_json::Value {
        let mut query_request = serde_json::Map::new();

        let mut json_array: Vec<serde_json::Value> = Vec::new();

        json_array.push(serde_json::json!({
        "type": "execute",
        "stmt": {
            "sql": sql,
            "args": params
                }
            }));

        if is_transacting {
            query_request.insert("baton".to_string(), serde_json::json!(baton));
        } else {
            json_array.push(serde_json::json!({
                "type": "close"
            }));
        }

        query_request.insert("requests".to_string(), json_array.into());

        serde_json::Value::from(query_request)
    }
}
