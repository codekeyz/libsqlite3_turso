use std::{
    collections::HashMap,
    error::Error,
    ffi::{c_char, c_int, c_void},
    fmt,
    sync::Mutex,
};

use crate::{
    transport::{self, RemoteSqliteResponse},
    utils::{convert_params_to_json, get_execution_result},
};

use lazy_static::lazy_static;

pub const SQLITE_OK: c_int = 0;
pub const SQLITE_ERROR: c_int = 1;
pub const SQLITE_MISUSE: c_int = 21;
pub const SQLITE_ROW: c_int = 100;
pub const SQLITE_DONE: c_int = 101;
pub const SQLITE_RANGE: c_int = 25;
pub const SQLITE_BUSY: c_int = 5;
pub const SQLITE_CANTOPEN: c_int = 14;

pub const SQLITE_INTEGER: c_int = 1;
pub const SQLITE_FLOAT: c_int = 2;
pub const SQLITE_TEXT: c_int = 3;
pub const SQLITE_NULL: c_int = 5;

pub const SQLITE_UPDATE: c_int = 23;
pub const SQLITE_INSERT: c_int = 18;
pub const SQLITE_DELETE: c_int = 9;

pub const SQLITE_NO_ACTIVE_TRANSACTION_ERR_MSG: &str = "No transaction is currently active.";
pub const SQLITE_ALREADY_ACTIVE_TRANSACTION_ERR_MSG: &str = "A transaction is already active.";

pub type SqliteHook = extern "C" fn(
    user_data: *mut c_void,  // User-provided data
    op: c_int,               // Operation: INSERT, UPDATE, DELETE
    db_name: *const c_char,  // Database name
    tbl_name: *const c_char, // Table name
    row_id: i64,             // Affected row ID
);

pub struct SqliteHookData {
    pub op: c_int,        // Operation type
    pub db_name: String,  // Database name
    pub tbl_name: String, // Table name
    pub row_id: i64,      // Affected row ID
}

#[derive(Debug, Clone)]
pub enum Value {
    Text(String), // TEXT
    Integer(i64), // INTEGER
    Real(f64),    // REAL
    Null,         // NULL
}

#[derive(Debug)]
pub struct SqliteError {
    pub message: String,
    pub code: c_int, //  defaults to SQLITE_ERROR
}

impl SqliteError {
    pub fn new(message: impl Into<String>, code: Option<c_int>) -> Self {
        Self {
            message: message.into(),
            code: code.unwrap_or(SQLITE_ERROR),
        }
    }
}

impl fmt::Display for SqliteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SQLite error (code {}): {}", self.code, self.message)
    }
}

lazy_static! {
    pub static ref ERROR_STACK: Mutex<Vec<(String, c_int)>> = Mutex::new(Vec::new());
}

#[repr(C)]
pub struct SQLite3 {
    pub connection: transport::DatabaseConnection, // Connection to the database
    pub last_insert_rowid: Mutex<Option<i64>>,     // Last inserted row ID
    pub transaction_baton: Mutex<Option<String>>,  // Baton for transaction management
    pub transaction_has_began: Mutex<bool>,        // Flag to check if a transaction has started
    pub update_hook: Mutex<Option<(SqliteHook, *mut c_void)>>, // Update hook callback
    pub insert_hook: Mutex<Option<(SqliteHook, *mut c_void)>>, // Insert hook callback
    pub delete_hook: Mutex<Option<(SqliteHook, *mut c_void)>>, // Delete hook callback
}

impl SQLite3 {
    pub fn trigger_hook(&self, data: SqliteHookData) {
        let hook = match data.op {
            SQLITE_UPDATE => &self.update_hook,
            SQLITE_INSERT => &self.insert_hook,
            SQLITE_DELETE => &self.delete_hook,
            _ => return,
        };
        let hook = hook.lock().unwrap();

        if let Some((callback, user_data)) = &*hook {
            let db_name_c = std::ffi::CString::new(data.db_name).unwrap();
            let tbl_name_c = std::ffi::CString::new(data.tbl_name).unwrap();

            // Call the registered callback
            callback(
                *user_data,
                data.op,
                db_name_c.as_ptr(),
                tbl_name_c.as_ptr(),
                data.row_id,
            );
        }
    }

    pub fn register_hook(
        &self,
        op: c_int,
        callback: Option<SqliteHook>,
        user_data: *mut c_void,
    ) -> c_int {
        let hook = match op {
            SQLITE_UPDATE => &self.update_hook,
            SQLITE_INSERT => &self.insert_hook,
            SQLITE_DELETE => &self.delete_hook,
            _ => return SQLITE_MISUSE,
        };

        let mut hook = hook.lock().unwrap();
        *hook = callback.map(|cb| (cb, user_data)); // Store the callback and user data

        SQLITE_OK
    }

    pub fn has_began_transaction(&self) -> bool {
        *self.transaction_has_began.lock().unwrap()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExecutionState {
    Prepared,      // Statement is prepared but not yet executed
    Executing,     // Statement is currently executing
    Row,           // A row of data is available (for SELECT queries)
    Done,          // Execution has completed successfully
    Error(String), // Execution encountered an error (with an optional message)
    Reset,         // Statement has been reset
}

#[repr(C)]
#[derive(Debug)]
pub struct SQLite3PreparedStmt {
    pub sql: String,                            // SQL statement as a CString
    pub param_count: c_int,                     // Number of parameters in the statement
    pub params: HashMap<i32, Value>,            // Bound parameters (index -> value)
    pub execution_state: Mutex<ExecutionState>, // Execution state
    pub result_rows: Mutex<Vec<Vec<Value>>>,    // Result rows
    pub current_row: Mutex<Option<usize>>,      // Index of the current row
    pub column_names: Vec<String>,              // Column names for the result set
    pub db: *mut SQLite3,                       // Pointer to the associated database
}

impl SQLite3PreparedStmt {
    pub fn new(db: *mut SQLite3, sql: &str) -> Self {
        SQLite3PreparedStmt {
            sql: sql.to_string(),
            param_count: 0,
            params: HashMap::new(),
            execution_state: Mutex::new(ExecutionState::Prepared),
            result_rows: Mutex::new(Vec::new()),
            current_row: Mutex::new(None),
            column_names: Vec::new(),
            db,
        }
    }
}

pub type SQLite3ExecCallback = Option<
    unsafe extern "C" fn(
        arg: *mut c_void,
        column_count: c_int,
        column_values: *mut *mut c_char,
        column_names: *mut *mut c_char,
    ) -> c_int,
>;

pub unsafe fn push_error(error: (String, c_int)) -> c_int {
    let mut stack = ERROR_STACK.lock().unwrap();
    let code = error.1;
    stack.push(error);
    code
}

pub unsafe fn get_latest_error() -> Option<(String, c_int)> {
    if let Ok(stack) = ERROR_STACK.lock() {
        stack.last().cloned()
    } else {
        None
    }
}

pub fn reset_txn_on_db(db: *mut SQLite3) -> c_int {
    let db = unsafe { &mut *db };

    if !db.has_began_transaction() {
        return SQLITE_OK;
    }

    *db.transaction_has_began.lock().unwrap() = false;
    db.transaction_baton.lock().unwrap().take();

    SQLITE_OK
}

pub fn iterate_rows(stmt: &mut SQLite3PreparedStmt) -> Result<c_int, Box<dyn Error>> {
    let result_rows = stmt.result_rows.lock().unwrap();
    let mut current_row = stmt.current_row.lock().unwrap();

    match *current_row {
        Some(row_index) if row_index + 1 < result_rows.len() => {
            *current_row = Some(row_index + 1);

            // Update state
            if let Ok(mut exec_state) = stmt.execution_state.lock() {
                *exec_state = ExecutionState::Row;
            }

            Ok(SQLITE_ROW)
        }
        Some(_) => {
            *current_row = None;

            // Update state
            if let Ok(mut exec_state) = stmt.execution_state.lock() {
                *exec_state = ExecutionState::Done;
            }

            Ok(SQLITE_DONE)
        }
        None if !result_rows.is_empty() => {
            *current_row = Some(0);

            // Update state
            if let Ok(mut exec_state) = stmt.execution_state.lock() {
                *exec_state = ExecutionState::Row;
            }

            Ok(SQLITE_ROW)
        }
        None => {
            // Update state
            if let Ok(mut exec_state) = stmt.execution_state.lock() {
                *exec_state = ExecutionState::Done;
            }

            Ok(SQLITE_DONE)
        }
    }
}

pub async fn handle_execute(db: *mut SQLite3, sql: &str) -> Result<c_int, SqliteError> {
    let mut stmt = SQLite3PreparedStmt::new(db, sql);

    match execute_stmt(&mut stmt).await {
        Ok(_) => Ok(SQLITE_OK),
        Err(e) => Err(e),
    }
}

pub async fn begin_tnx_on_db(db: *mut SQLite3, sql: &str) -> Result<c_int, SqliteError> {
    let db = unsafe { &mut *db };

    if db.has_began_transaction() {
        return Err(SqliteError::new(
            SQLITE_ALREADY_ACTIVE_TRANSACTION_ERR_MSG.to_string(),
            Some(SQLITE_BUSY),
        ));
    }

    let baton_value = db.connection.get_transaction_baton(&sql).await?;
    db.transaction_baton.lock().unwrap().replace(baton_value);
    *db.transaction_has_began.lock().unwrap() = true;

    Ok(SQLITE_OK)
}

pub async fn commit_tnx_on_db(db: *mut SQLite3, sql: &str) -> Result<c_int, SqliteError> {
    let db = unsafe { &mut *db };

    if !db.has_began_transaction() {
        return Err(SqliteError::new(
            SQLITE_NO_ACTIVE_TRANSACTION_ERR_MSG,
            Some(SQLITE_ERROR),
        ));
    }

    execute_sql_and_params(db, &sql, vec![]).await?;

    db.transaction_baton.lock().unwrap().take();

    reset_txn_on_db(db);

    Ok(SQLITE_OK)
}

pub async fn execute_stmt(stmt: &mut SQLite3PreparedStmt) -> Result<c_int, SqliteError> {
    let db: &mut SQLite3 = unsafe { &mut *stmt.db };

    let params = convert_params_to_json(&stmt.params);
    let response = execute_sql_and_params(db, &stmt.sql, params).await?;
    let response = get_execution_result(db, &response)?;

    stmt.column_names = response.cols.iter().map(|col| col.name.clone()).collect();

    let mut result_rows = stmt.result_rows.lock().unwrap();
    *result_rows = response
        .rows
        .iter()
        .map(|row| {
            let result = row
                .iter()
                .map(|row| {
                    if row.value.is_none() {
                        return Value::Null;
                    }

                    let value = row.value.as_ref().unwrap();

                    match row.r#type.as_str() {
                        "integer" => match &value {
                            serde_json::Value::String(s) => {
                                Value::Integer(s.parse::<i64>().unwrap_or(0))
                            }
                            serde_json::Value::Number(n) => Value::Integer(n.as_i64().unwrap_or(0)),
                            _ => Value::Integer(0),
                        },
                        "float" => Value::Real(value.as_f64().unwrap_or(0.0)),
                        "text" => Value::Text(value.as_str().unwrap_or("").to_string()),
                        "null" => Value::Null,
                        _ => Value::Null,
                    }
                })
                .collect();

            result
        })
        .collect();

    Ok(SQLITE_OK)
}

async fn execute_sql_and_params(
    db: &mut SQLite3,
    sql: &str,
    params: Vec<serde_json::Value>,
) -> Result<RemoteSqliteResponse, SqliteError> {
    if let transport::ActiveStrategy::Websocket = db.connection.strategy {
        let mut request = db.connection.get_json_request(db, sql, &params);
        match db.connection.send(&mut request).await {
            Ok(response) => return Ok(response),
            Err(err) => {
                db.connection.strategy = transport::ActiveStrategy::Http;
                return Err(err);
            }
        }
    }

    let request = &mut db.connection.get_json_request(db, sql, &params);
    let result = db.connection.send(request).await;

    if let Err(e) = result {
        return Err(SqliteError::new(e.to_string(), Some(SQLITE_ERROR)));
    }

    Ok(result.unwrap())
}
