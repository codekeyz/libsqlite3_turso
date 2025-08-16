use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use crate::{
    sqlite::{SqliteError, SQLITE_ERROR},
    transport::{
        LibsqlInterface, RemoteSQLiteResult, RemoteSQliteResultType, RemoteSqliteResponse,
        TursoConfig,
    },
    utils::get_tokio,
};
use futures_util::{sink::SinkExt, stream::SplitSink, StreamExt};
use tokio_tungstenite::{
    tungstenite::{Message, Utf8Bytes},
    MaybeTlsStream, WebSocketStream,
};

use serde_json::Value;
use tokio::{
    net::TcpStream,
    sync::{oneshot, Mutex},
};

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);
static STREAM_ID: AtomicU64 = AtomicU64::new(1);

#[derive(PartialEq)]
enum WebSocketConnState {
    Connected,
    Disconnected,
}

pub struct WebSocketStrategy {
    turso_config: Arc<TursoConfig>,
    bus: ResponseBus,
    websocket_handle: Option<
        SplitSink<
            WebSocketStream<MaybeTlsStream<TcpStream>>,
            tokio_tungstenite::tungstenite::Message,
        >,
    >,
    websocket_state: Arc<Mutex<WebSocketConnState>>,
}

impl WebSocketStrategy {
    pub fn new(turso_config: Arc<TursoConfig>) -> Self {
        Self {
            turso_config,
            bus: ResponseBus::new(),
            websocket_handle: None,
            websocket_state: Arc::new(Mutex::new(WebSocketConnState::Disconnected)),
        }
    }

    fn next_request_id() -> i32 {
        REQUEST_ID.fetch_add(1, Ordering::Relaxed) as i32
    }

    fn next_stream_id() -> i32 {
        STREAM_ID.fetch_add(1, Ordering::Relaxed) as i32
    }

    async fn open_stream(&mut self) -> Result<(i32, ResponseBus), SqliteError> {
        let (writer, bus) = self.get_client().await?;
        let request_id = WebSocketStrategy::next_request_id();
        let stream_id = WebSocketStrategy::next_stream_id();

        let request = serde_json::json!({
            "type": "request",
            "request_id": request_id,
            "request": {
            "type": "open_stream",
            "stream_id": stream_id,
        }
        });

        writer
            .send(Message::Text(Utf8Bytes::from(request.to_string())))
            .await
            .map_err(|e| {
                SqliteError::new(
                    format!("Failed to send open_stream message: {}", e),
                    Some(SQLITE_ERROR),
                )
            })?;

        let id = format!("request_id:{}", request_id);
        bus.wait_for(id.as_str()).await?;

        Ok((stream_id, bus))
    }

    pub async fn connect(&mut self) -> Result<(), SqliteError> {
        let url = format!("wss://{}", self.turso_config.db_url);
        if cfg!(debug_assertions) {
            println!("Connecting to WebSocket at {}", url);
        }

        let (socket, _) = tokio_tungstenite::connect_async(url).await.map_err(|e| {
            SqliteError::new(
                format!("Failed to connect to WebSocket: {}", e),
                Some(SQLITE_ERROR),
            )
        })?;
        let (mut writer, mut reader) = socket.split();

        let bus = self.bus.clone();
        let websocket_state = self.websocket_state.clone();

        get_tokio().spawn(async move {
            while let Some(message) = reader.next().await {
                match message {
                    Err(_) | Ok(Message::Close(_)) => {
                        let mut state = websocket_state.lock().await;
                        *state = WebSocketConnState::Disconnected;
                        break;
                    }
                    _ => (),
                }

                let message = message.unwrap();
                let value: Value = match message {
                    Message::Text(text) => match serde_json::from_str(&text) {
                        Ok(value) => value,
                        Err(_) => {
                            continue;
                        }
                    },
                    Message::Binary(binary) => {
                        match serde_json::from_slice(&binary) {
                            Ok(value) => value,
                            Err(e) => {
                                eprintln!(
                                    "Failed to parse WebSocket binary message as JSON: {}",
                                    e
                                );
                                continue;
                            }
                        }
                        continue;
                    }
                    _ => {
                        eprintln!("Received unsupported WebSocket message type: {:?}", message);
                        continue;
                    }
                };

                // store the key and value as {key:value}

                let key = {
                    if value.get("request_id").is_some() {
                        format!(
                            "request_id:{}",
                            value.get("request_id").unwrap().as_i64().unwrap()
                        )
                    } else if value.get("id").is_some() {
                        format!("id:{}", value.get("id").unwrap().as_i64().unwrap())
                    } else {
                        format!("type:{}", value.get("type").unwrap().as_str().unwrap())
                    }
                };

                bus.respond(key.as_str(), value.clone()).await;
            }
        });

        let json = serde_json::json!({
            "type": "hello",
            "jwt": self.turso_config.db_token,
        });

        writer
            .send(tokio_tungstenite::tungstenite::Message::Text(
                Utf8Bytes::from(json.to_string()),
            ))
            .await
            .map_err(|e| {
                SqliteError::new(
                    format!("Failed to send initial message over WebSocket: {}", e),
                    Some(SQLITE_ERROR),
                )
            })?;

        let result = self.bus.wait_for("type:hello_ok").await;
        if result.is_err() {
            return Err(SqliteError::new(
                "Failed to validate database URL & Token. Try again".to_string(),
                Some(SQLITE_ERROR),
            ));
        }

        self.websocket_handle = Some(writer);
        self.websocket_state = Arc::new(Mutex::new(WebSocketConnState::Connected));
        Ok(())
    }

    async fn get_client(
        &mut self,
    ) -> Result<
        (
            &mut SplitSink<
                WebSocketStream<MaybeTlsStream<TcpStream>>,
                tokio_tungstenite::tungstenite::Message,
            >,
            ResponseBus,
        ),
        SqliteError,
    > {
        let state = self.websocket_state.lock().await;
        let need_connect =
            self.websocket_handle.is_none() || *state == WebSocketConnState::Disconnected;
        drop(state);

        if need_connect {
            self.connect().await?;
        }

        Ok((self.websocket_handle.as_mut().unwrap(), self.bus.clone()))
    }
}

impl LibsqlInterface for WebSocketStrategy {
    async fn get_transaction_baton(&mut self, sql: &str) -> Result<String, SqliteError> {
        // Implementation for WebSocket transport
        unimplemented!()
    }

    async fn send(
        &mut self,
        request: &mut serde_json::Value,
    ) -> Result<RemoteSqliteResponse, SqliteError> {
        if let WebSocketConnState::Disconnected = *self.websocket_state.lock().await {
            return Err(SqliteError::new(
                "WebSocket connection is disconnected".to_string(),
                Some(SQLITE_ERROR),
            ));
        }

        let (stream_id, bus) = self.open_stream().await?;
        let request_id = WebSocketStrategy::next_request_id();
        request["stream_id"] = serde_json::Value::from(stream_id);

        let request = serde_json::json!({
            "type": "request",
            "request_id": request_id,
            "request": request
        });

        let writer = self.websocket_handle.as_mut().unwrap();

        if cfg!(debug_assertions) {
            println!("Sending request over WebSocket: {:?}", request);
        }

        writer
            .send(tokio_tungstenite::tungstenite::Message::Text(
                Utf8Bytes::from(request.to_string()),
            ))
            .await
            .unwrap();

        let result = bus
            .wait_for(format!("request_id:{}", request_id).as_str())
            .await?;

        let parsed: RemoteSQliteResultType = serde_json::from_value(result).map_err(|e| {
            SqliteError::new(
                format!("Failed to parse response: {}", e),
                Some(SQLITE_ERROR),
            )
        })?;
        let result = parsed.response;
        if let RemoteSQLiteResult::Error { message, code } = result {
            return Err(SqliteError::new(
                format!("Remote SQLite error (code {}): {}", code, message),
                Some(SQLITE_ERROR),
            ));
        }
        if let RemoteSQLiteResult::Close = result {
            return Err(SqliteError::new(
                "Remote SQLite closed the connection unexpectedly".to_string(),
                None,
            ));
        }

        if let RemoteSQLiteResult::Execute { result } = result {
            return Ok(RemoteSqliteResponse {
                baton: None,
                results: vec![RemoteSQliteResultType {
                    response: RemoteSQLiteResult::Execute { result },
                }],
            });
        }

        // Here you would typically wait for a response and parse it
        // For now, we return a placeholder response
        Ok(RemoteSqliteResponse {
            baton: None,
            results: vec![],
        })
    }

    fn get_json_request(
        &self,
        sql: &str,
        params: &Vec<serde_json::Value>,
        baton: Option<&String>,
        is_transacting: bool,
    ) -> serde_json::Value {
        serde_json::json!({
            "type": "execute",
            "stmt": {
                "sql": sql,
                "args": params
            }
        })
    }
}

#[derive(Clone)]
struct ResponseBus {
    map: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
}

impl ResponseBus {
    pub fn new() -> Self {
        Self {
            map: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn wait_for(&self, id: &str) -> Result<Value, SqliteError> {
        let (tx, rx) = oneshot::channel();
        self.map.lock().await.insert(id.to_string(), tx);

        match tokio::time::timeout(Duration::from_secs(10), rx).await {
            Ok(result) => match result {
                Ok(value) => Ok(value),
                Err(_) => Err(SqliteError::new(
                    "Failed to receive response".to_string(),
                    Some(SQLITE_ERROR),
                )),
            },
            Err(_) => Err(SqliteError::new(
                "Response timed out".to_string(),
                Some(SQLITE_ERROR),
            )),
        }
    }

    pub async fn respond(&self, id: &str, value: Value) {
        if let Some(sender) = self.map.lock().await.remove(id) {
            let _ = sender.send(value);
        }
    }
}
