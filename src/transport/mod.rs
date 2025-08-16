use std::sync::Arc;

use serde::Deserialize;

use crate::{
    auth::DbAuthStrategy,
    sqlite::{SQLite3, SqliteError, SQLITE_CANTOPEN},
    transport::{http::HttpStrategy, wss::WebSocketStrategy},
};

mod http;
mod response_bus;
mod wss;

#[derive(Debug, Deserialize, Clone)]
pub struct TursoConfig {
    pub db_url: String,
    pub db_token: String,
}

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
    Error { message: String, code: String },
    Close,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RemoteCol {
    pub name: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RemoteRow {
    pub r#type: String,
    pub value: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct QueryResult {
    pub cols: Vec<RemoteCol>,
    pub rows: Vec<Vec<RemoteRow>>,
    pub last_insert_rowid: Option<String>,
}

pub trait LibsqlInterface {
    fn get_json_request(
        &self,
        sql: &str,
        params: Vec<serde_json::Value>,
        baton: Option<&String>,
        is_transacting: bool,
    ) -> serde_json::Value;

    async fn get_transaction_baton(&mut self, sql: &str) -> Result<String, SqliteError>;

    async fn send(
        &mut self,
        request: serde_json::Value,
    ) -> Result<RemoteSqliteResponse, SqliteError>;
}

#[derive(PartialEq)]
pub enum ActiveStrategy {
    Http,
    Websocket,
}

pub struct DatabaseConnection {
    http: HttpStrategy,
    websocket: WebSocketStrategy,
    pub strategy: ActiveStrategy,
}

impl DatabaseConnection {
    pub async fn open(
        db_name: &str,
        auth: Box<dyn DbAuthStrategy>,
        strategy: ActiveStrategy,
    ) -> Result<Self, SqliteError> {
        let reqwest_client = reqwest::Client::builder()
            .user_agent("libsqlite3_turso/1.0.0")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap();

        let turso_config = auth.resolve(db_name, &reqwest_client).await;
        if turso_config.is_err() {
            let error = turso_config.unwrap_err();
            return Err(SqliteError::new(error.to_string(), Some(SQLITE_CANTOPEN)));
        }

        let turso_config = Arc::new(turso_config.unwrap());

        let http = HttpStrategy::new(reqwest_client, turso_config.clone());
        let mut websocket = WebSocketStrategy::new(turso_config.clone());

        if let ActiveStrategy::Websocket = strategy {
            websocket.connect().await?;

            //wait 10 seconds for the server to respond
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;

            if cfg!(debug_assertions) {
                println!("WebSocket connection established for {}", db_name);
            }
        }

        Ok(Self {
            http,
            websocket,
            strategy,
        })
    }

    pub async fn get_transaction_baton(&mut self, sql: &str) -> Result<String, SqliteError> {
        match self.strategy {
            ActiveStrategy::Http => self.http.get_transaction_baton(sql).await,
            ActiveStrategy::Websocket => self.websocket.get_transaction_baton(sql).await,
        }
    }

    pub async fn send(
        &mut self,
        request: serde_json::Value,
    ) -> Result<RemoteSqliteResponse, SqliteError> {
        match self.strategy {
            ActiveStrategy::Http => self.http.send(request).await,
            ActiveStrategy::Websocket => self.websocket.send(request).await,
        }
    }

    pub fn get_json_request(
        &self,
        db: &SQLite3,
        sql: &str,
        params: Vec<serde_json::Value>,
    ) -> serde_json::Value {
        let baton_str = {
            let baton = db.transaction_baton.lock().unwrap();
            baton.as_ref().map(|s| s.as_str()).map(|s| s.to_owned())
        };
        let has_begun_transaction = db.has_began_transaction();

        match self.strategy {
            ActiveStrategy::Http => {
                self.http
                    .get_json_request(sql, params, baton_str.as_ref(), has_begun_transaction)
            }
            ActiveStrategy::Websocket => self.websocket.get_json_request(
                sql,
                params,
                baton_str.as_ref(),
                has_begun_transaction,
            ),
        }
    }
}
