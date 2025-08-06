use std::{
    collections::HashMap,
    error::Error,
    ffi::{c_char, c_int, c_void},
    sync::Mutex,
};

use crate::{
    proxy::{
        convert_params_to_json, execute_sql_and_params, get_execution_result,
        get_transaction_baton, QueryResult,
    },
    utils::TursoConfig,
};

pub const SQLITE_OK: c_int = 0;
pub const SQLITE_ERROR: c_int = 1;
pub const SQLITE_MISUSE: c_int = 21;
pub const SQLITE_ROW: c_int = 100;
pub const SQLITE_DONE: c_int = 101;
pub const SQLITE_RANGE: c_int = 25;
pub const SQLITE_BUSY: c_int = 5;

pub const SQLITE_INTEGER: c_int = 1;
pub const SQLITE_FLOAT: c_int = 2;
pub const SQLITE_TEXT: c_int = 3;
pub const SQLITE_NULL: c_int = 5;

pub const SQLITE_UPDATE: c_int = 23;
pub const SQLITE_INSERT: c_int = 18;
pub const SQLITE_DELETE: c_int = 9;

pub const SQLITE_NO_ACTIVE_TRANSACTION_ERR_MSG: &str = "No transaction is currently active.";
pub const SQLITE_ALREADY_ACTIVE_TRANSACTION_ERR_MSG: &str = "A transaction is already active.";

// Type alias for the update hook callback
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

#[repr(C)]
pub struct SQLite3 {
    pub client: reqwest::Client, // HTTP client for making requests
    pub last_insert_rowid: Mutex<Option<i64>>, // Last inserted row ID
    pub error_stack: Mutex<Vec<(String, c_int)>>, // Stack to store error messages
    pub transaction_baton: Mutex<Option<String>>, // Baton for transaction management
    pub update_hook: Mutex<Option<(SqliteHook, *mut c_void)>>, // Update hook callback
    pub insert_hook: Mutex<Option<(SqliteHook, *mut c_void)>>, // Insert hook callback
    pub delete_hook: Mutex<Option<(SqliteHook, *mut c_void)>>, // Delete hook callback
    pub turso_config: TursoConfig, // Configuration for Turso
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

    pub fn transaction_active(&self) -> bool {
        self.transaction_baton.lock().unwrap().is_some()
    }
}

#[derive(Debug, PartialEq, Eq)] // Traits for debugging and comparison
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

async unsafe fn execute_stmt(
    stmt: &mut SQLite3PreparedStmt,
) -> Result<QueryResult, Box<dyn Error>> {
    let db: &SQLite3 = &*stmt.db;
    let baton_str = {
        let baton = db.transaction_baton.lock().unwrap();
        baton.as_ref().map(|s| s.as_str()).map(|s| s.to_owned())
    };

    let params = convert_params_to_json(&stmt.params);
    let response = execute_sql_and_params(db, &stmt.sql, params, baton_str.as_ref()).await?;

    let result = get_execution_result(db, &response)?;

    Ok(result.clone())
}

async unsafe fn execute_stmt_and_populate_result_rows(
    stmt: &mut SQLite3PreparedStmt,
) -> Result<c_int, Box<dyn Error>> {
    let response = execute_stmt(stmt).await?;
    let mut result_rows = stmt.result_rows.lock().unwrap();

    let rows = response.rows;
    let columns = response.cols;
    stmt.column_names = columns.iter().map(|col| col.name.clone()).collect();

    *result_rows = rows
        .iter()
        .map(|row| {
            let result = row
                .iter()
                .map(|row| match row.r#type.as_str() {
                    "integer" => match &row.value {
                        serde_json::Value::String(s) => {
                            Value::Integer(s.parse::<i64>().unwrap_or(0))
                        }
                        serde_json::Value::Number(n) => Value::Integer(n.as_i64().unwrap_or(0)),
                        _ => Value::Integer(0),
                    },
                    "float" => Value::Real(row.value.as_f64().unwrap_or(0.0)),
                    "text" => Value::Text(row.value.as_str().unwrap_or("").to_string()),
                    "null" => Value::Null,
                    _ => Value::Null,
                })
                .collect();

            result
        })
        .collect();

    Ok(SQLITE_OK)
}

pub async unsafe fn handle_select(stmt: &mut SQLite3PreparedStmt) -> Result<c_int, Box<dyn Error>> {
    let needs_execution = {
        let result_rows = stmt.result_rows.lock().map_err(|_| "lock error")?;
        result_rows.is_empty()
    };

    if needs_execution {
        if let Err(err) = execute_stmt_and_populate_result_rows(stmt).await {
            return Err(err);
        }
    }

    let result_rows = stmt.result_rows.lock().unwrap();
    let mut current_row = stmt.current_row.lock().unwrap();

    // Handle row iteration
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

pub async unsafe fn handle_insert(stmt: &mut SQLite3PreparedStmt) -> Result<c_int, Box<dyn Error>> {
    match execute_stmt(stmt).await {
        Ok(_) => Ok(SQLITE_OK),
        Err(e) => Err(e),
    }
}

pub fn get_latest_error(db: &SQLite3) -> Option<(String, c_int)> {
    if let Ok(stack) = db.error_stack.lock() {
        stack.last().cloned()
    } else {
        None
    }
}

pub fn reset_txn_on_db(db: *mut SQLite3) -> c_int {
    if db.is_null() {
        return SQLITE_MISUSE;
    }

    let db = unsafe { &mut *db };

    if !db.transaction_active() {
        return SQLITE_OK;
    }

    db.transaction_baton.lock().unwrap().take();

    SQLITE_OK
}

pub fn push_error(db: *mut SQLite3, error: (String, c_int)) {
    if db.is_null() {
        return;
    }

    // Safety: Convert the raw pointer to a mutable reference
    let db = unsafe { &mut *db };

    if let Ok(mut stack) = db.error_stack.lock() {
        stack.push(error);
    }
}

pub async unsafe fn handle_execute(db: *mut SQLite3, sql: &str) -> Result<c_int, Box<dyn Error>> {
    if db.is_null() {
        return Err("Database pointer is null".into());
    }

    let db = &mut *db;
    let baton = db.transaction_baton.lock().unwrap();

    match execute_sql_and_params(db, sql, vec![], baton.as_ref()).await {
        Ok(_) => Ok(SQLITE_OK),
        Err(e) => Err(e),
    }
}

pub async fn begin_tnx_on_db(db: *mut SQLite3) -> Result<c_int, Box<dyn Error>> {
    if db.is_null() {
        return Err("Database pointer is null".into());
    }

    let db = unsafe { &mut *db };

    if db.transaction_active() {
        push_error(
            db,
            (
                SQLITE_ALREADY_ACTIVE_TRANSACTION_ERR_MSG.to_string(),
                SQLITE_BUSY,
            ),
        );
        return Err("Database is busy".into());
    }

    let baton_value = get_transaction_baton(&db.client, &db.turso_config).await?;
    db.transaction_baton.lock().unwrap().replace(baton_value);

    Ok(SQLITE_OK)
}

pub async fn commit_tnx_on_db(db: *mut SQLite3) -> Result<c_int, Box<dyn Error>> {
    if db.is_null() {
        return Err("Database pointer is null".into());
    }

    let db = unsafe { &mut *db };

    if !db.transaction_active() {
        push_error(
            db,
            (
                SQLITE_NO_ACTIVE_TRANSACTION_ERR_MSG.to_string(),
                SQLITE_ERROR,
            ),
        );
        return Err("No active transaction to commit".into());
    }

    let baton = db.transaction_baton.lock().unwrap().clone();

    execute_sql_and_params(db, "COMMIT", vec![], baton.as_ref()).await?;

    db.transaction_baton.lock().unwrap().take();

    reset_txn_on_db(db);

    Ok(SQLITE_OK)
}
