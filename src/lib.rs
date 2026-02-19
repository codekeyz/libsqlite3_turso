use regex::Regex;
use std::{
    collections::HashMap,
    ffi::{c_int, c_uint, c_void, CStr, CString},
    os::raw::c_char,
    slice,
    sync::Mutex,
};

use sqlite::{
    push_error, reset_txn_on_db, ExecutionState, SQLite3, SQLite3ExecCallback, SQLite3PreparedStmt,
    Value, SQLITE_BUSY, SQLITE_CANTOPEN, SQLITE_DONE, SQLITE_ERROR, SQLITE_FLOAT, SQLITE_INTEGER,
    SQLITE_MISUSE, SQLITE_NULL, SQLITE_OK, SQLITE_RANGE, SQLITE_TEXT,
};

use crate::{
    auth::{DbAuthStrategy, EnvVarStrategy, GlobeStrategy},
    sqlite::get_latest_error,
    utils::{
        count_parameters, execute_async_task, get_tokio, is_aligned, sql_is_begin_transaction,
        sql_is_commit, sql_is_pragma, sql_is_rollback,
    },
};

mod auth;
mod sqlite;
mod transport;
mod utils;

#[no_mangle]
pub extern "C" fn sqlite3_libversion_number() -> c_int {
    3037000 // This represents SQLite version 3.37.0
}

#[no_mangle]
pub extern "C" fn sqlite3_libversion() -> *const c_char {
    let version = CString::new("3.37.0").unwrap();
    version.into_raw()
}

#[no_mangle]
pub extern "C" fn sqlite3_sourceid() -> *const c_char {
    let id = CString::new("2022-01-06 13:25:4 libsqlite3_turso").unwrap();
    id.into_raw()
}

#[no_mangle]
pub unsafe extern "C" fn sqlite3_initialize() -> c_int {
    SQLITE_OK
}

#[no_mangle]
pub unsafe extern "C" fn sqlite3_open_v2(
    filename: *const c_char,
    db: *mut *mut SQLite3,
    _: c_int,
    _: *const c_char,
) -> c_int {
    if filename.is_null() || db.is_null() {
        return SQLITE_ERROR;
    }

    let db_name = CStr::from_ptr(filename).to_str().unwrap();
    if db_name.contains(":memory") {
        return push_error((
            "In-memory databases are not supported".to_string(),
            SQLITE_CANTOPEN,
        ));
    }

    // Check if running in Globe environment
    let auth_strategy: Box<dyn DbAuthStrategy> = {
        let is_globe_env = std::env::var("GLOBE")
            .and_then(|v| Ok(v == "1"))
            .unwrap_or(false);
        if is_globe_env {
            Box::new(GlobeStrategy)
        } else {
            Box::new(EnvVarStrategy)
        }
    };
    let connection =
        get_tokio().block_on(transport::DatabaseConnection::open(db_name, auth_strategy));
    if let Some(error) = connection.as_ref().err() {
        return push_error((error.to_string(), SQLITE_CANTOPEN));
    }

    let mock_db = Box::into_raw(Box::new(SQLite3 {
        connection: connection.unwrap(),
        transaction_baton: Mutex::new(None),
        last_insert_rowid: Mutex::new(None),
        rows_written: Mutex::new(None),
        transaction_has_began: Mutex::new(false),
        delete_hook: Mutex::new(None),
        insert_hook: Mutex::new(None),
        update_hook: Mutex::new(None),
    }));

    *db = mock_db;

    SQLITE_OK
}

#[no_mangle]
pub extern "C" fn sqlite3_extended_result_codes(db: *mut SQLite3, _onoff: i32) -> i32 {
    if !is_aligned(db) {
        return SQLITE_CANTOPEN;
    }

    SQLITE_OK
}

#[no_mangle]
pub unsafe extern "C" fn sqlite3_prepare_v3(
    _db: *mut SQLite3,                      // Database handle
    _sql: *const c_char,                    // SQL statement string
    byte_len: usize,                        // Length of zSql in bytes.
    prep_flag: c_uint,                      // Preparation flags
    pp_stmt: *mut *mut SQLite3PreparedStmt, // OUT: Prepared statement handle
    pz_tail: *mut *const c_char,            // OUT: Unprocessed SQL string
) -> c_int {
    if !is_aligned(_db) {
        return SQLITE_ERROR;
    }

    if prep_flag != 0 {
        return push_error((
            "Persisted prepared statements not supported yet.".to_string(),
            SQLITE_MISUSE,
        ));
    }

    let bytes = slice::from_raw_parts(_sql as *const u8, byte_len);
    let sql = match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => {
            eprintln!("sqlite3_prepare_v3: Failed to convert SQL statement to string");
            return SQLITE_ERROR;
        }
    };

    let param_count = count_parameters(&sql);

    // Mock unparsed portion of SQL
    if !pz_tail.is_null() {
        let parsed_sql = CString::new(sql.as_str()).unwrap();
        unsafe { *pz_tail = _sql.add(parsed_sql.to_bytes().len()) };
    }

    // Allocate a mock prepared statement
    let stmt = Box::new(SQLite3PreparedStmt {
        db: _db,
        sql: sql,
        param_count,
        params: HashMap::new(), // Initialize an empty map for parameters
        execution_state: Mutex::new(ExecutionState::Prepared), // Start in the "Prepared" state
        result_rows: Mutex::new(vec![]), // Initialize an empty result set
        current_row: Mutex::new(None), // No current row initially
        column_names: Vec::new(),
    });
    *pp_stmt = Box::into_raw(stmt);

    SQLITE_OK
}

#[no_mangle]
pub extern "C" fn sqlite3_bind_parameter_count(stmt: *mut SQLite3PreparedStmt) -> c_int {
    if stmt.is_null() {
        return 0;
    }
    let stmt = unsafe { &*stmt };
    stmt.param_count
}

#[no_mangle]
pub extern "C" fn sqlite3_finalize(stmt: *mut SQLite3PreparedStmt) -> c_int {
    if stmt.is_null() {
        return SQLITE_ERROR;
    }

    unsafe {
        let _ = Box::from_raw(stmt);
    }

    // Return success code
    SQLITE_OK
}

#[no_mangle]
pub unsafe extern "C" fn sqlite3_bind_text(
    stmt_ptr: *mut SQLite3PreparedStmt, // Prepared statement handle
    index: c_int,                       // Index of the parameter to bind
    value: *const c_char,               // Value to bind
    byte_len: usize,                    // Length of the value in bytes
    _: Option<unsafe extern "C" fn(ptr: *mut c_void)>, // Destructor (ignored in mock)
) -> c_int {
    if stmt_ptr.is_null() || value.is_null() {
        return SQLITE_MISUSE;
    }

    let stmt = &mut *stmt_ptr; // Mutable access required

    if index <= 0 || index > stmt.param_count {
        return SQLITE_RANGE;
    }

    let bytes = slice::from_raw_parts(value as *const u8, byte_len);
    let text = match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => {
            eprintln!("sqlite3_bind_text: invalid UTF-8 at index {}", index);
            return SQLITE_MISUSE;
        }
    };

    stmt.params.insert(index, Value::Text(text));
    SQLITE_OK
}

#[no_mangle]
pub unsafe extern "C" fn sqlite3_bind_double(
    stmt_ptr: *mut SQLite3PreparedStmt, // Pointer to the prepared statement
    index: c_int,                       // Index of the parameter to bind
    value: f64,                         // Double value to bind
    _: Option<unsafe extern "C" fn(ptr: *mut c_void)>,
) -> i32 {
    if stmt_ptr.is_null() {
        return SQLITE_MISUSE;
    }

    let stmt = &mut *stmt_ptr;
    if index <= 0 || index > stmt.param_count {
        return SQLITE_RANGE;
    }

    stmt.params.insert(index, Value::Real(value));
    SQLITE_OK
}

#[no_mangle]
pub unsafe extern "C" fn sqlite3_bind_int64(
    stmt_ptr: *mut SQLite3PreparedStmt, // Pointer to the prepared statement
    index: i32,                         // 1-based index of the parameter
    value: i64,                         // 64-bit integer value to bind
    _: Option<unsafe extern "C" fn(ptr: *mut c_void)>, // Destructor (ignored in mock)
) -> i32 {
    if stmt_ptr.is_null() {
        return SQLITE_MISUSE;
    }

    let stmt = &mut *stmt_ptr; // Mutable access required
    if index <= 0 || index > stmt.param_count {
        return SQLITE_RANGE;
    }

    stmt.params.insert(index, Value::Integer(value));
    SQLITE_OK
}

#[no_mangle]
pub extern "C" fn sqlite3_bind_null(stmt_ptr: *mut SQLite3PreparedStmt, index: c_int) -> c_int {
    if stmt_ptr.is_null() {
        return SQLITE_MISUSE;
    }

    let stmt = unsafe { &mut *stmt_ptr };

    if index <= 0 || index > stmt.param_count {
        return SQLITE_RANGE;
    }

    stmt.params.insert(index, Value::Null);
    SQLITE_OK
}

#[no_mangle]
pub unsafe extern "C" fn sqlite3_step(stmt_ptr: *mut SQLite3PreparedStmt) -> c_int {
    if stmt_ptr.is_null() {
        return SQLITE_MISUSE;
    }

    let stmt = &mut *stmt_ptr;

    let mut exec_state = match stmt.execution_state.lock() {
        Ok(guard) => guard,
        Err(_) => return SQLITE_ERROR,
    };

    match *exec_state {
        ExecutionState::Prepared | ExecutionState::Row => {
            if *exec_state == ExecutionState::Prepared {
                *exec_state = ExecutionState::Executing;
            }
        }
        ExecutionState::Done => return SQLITE_DONE,
        _ => return SQLITE_MISUSE,
    }
    drop(exec_state);

    let needs_execution = stmt.result_rows.lock().unwrap().is_empty();
    if needs_execution {
        let sql = stmt.sql.to_uppercase();
        let sql_result_code = {
            if sql_is_begin_transaction(&sql) {
                execute_async_task(sqlite::begin_tnx_on_db(stmt.db, &sql))
            } else if sql_is_commit(&sql) {
                execute_async_task(sqlite::commit_tnx_on_db(stmt.db, &sql))
            } else {
                execute_async_task(sqlite::execute_stmt(stmt))
            }
        };

        if sql_result_code != SQLITE_OK {
            return sql_result_code;
        }
    }

    let sql_result_code = sqlite::iterate_rows(stmt);
    if let Err(error) = sql_result_code {
        push_error((error.to_string(), SQLITE_ERROR));
        return SQLITE_ERROR;
    }

    sql_result_code.unwrap()
}

#[no_mangle]
pub extern "C" fn sqlite3_column_count(stmt: *mut SQLite3PreparedStmt) -> i32 {
    if !is_aligned(stmt) {
        return 0;
    }

    let stmt = unsafe { &mut *stmt };

    stmt.column_names.len() as i32
}

#[no_mangle]
pub unsafe extern "C" fn sqlite3_last_insert_rowid(_db: *mut SQLite3) -> i64 {
    if _db.is_null() {
        return 0;
    }

    let db = &mut *_db;

    if let Ok(last_insert_rowid) = db.last_insert_rowid.lock() {
        if let Some(row_id) = *last_insert_rowid {
            return row_id;
        }
    }

    0
}

#[no_mangle]
pub extern "C" fn sqlite3_reset(stmt: *mut SQLite3PreparedStmt) -> c_int {
    if stmt.is_null() {
        return SQLITE_ERROR;
    }

    // Safely convert the raw pointer to a mutable reference
    let stmt = unsafe { &mut *stmt };

    // Reset execution state to Prepared
    if let Ok(mut exec_state) = stmt.execution_state.lock() {
        *exec_state = ExecutionState::Prepared; // Reset to initial state
    }

    // Clear parameters
    stmt.params.clear();

    // Clear result
    if let Ok(mut result_rows) = stmt.result_rows.lock() {
        result_rows.clear(); // Remove all previously bound parameters
    }

    stmt.column_names.clear();

    SQLITE_OK
}

#[no_mangle]
pub unsafe extern "C" fn sqlite3_close_v2(db: *mut SQLite3) -> c_int {
    if !is_aligned(db) {
        return SQLITE_OK;
    }

    let db = unsafe { &mut *db };

    reset_txn_on_db(db);

    drop(Box::from_raw(db));

    SQLITE_OK
}

#[no_mangle]
pub unsafe extern "C" fn sqlite3_extended_errcode(_: *mut SQLite3) -> c_int {
    let error_stack = get_latest_error();
    if let Some((_, code)) = error_stack {
        return code;
    }

    SQLITE_OK
}

#[no_mangle]
pub unsafe extern "C" fn sqlite3_changes(db: *mut SQLite3) -> c_int {
    if !is_aligned(db) {
        return SQLITE_OK;
    }
    let db = &mut *db;

    if let Ok(rows_written) = db.rows_written.lock() {
        if rows_written.is_some() {
            return rows_written.unwrap() as c_int;
        }
    }

    0
}

#[no_mangle]
pub unsafe extern "C" fn sqlite3_errmsg(_: *mut SQLite3) -> *const c_char {
    if let Some(error_entry) = sqlite::get_latest_error() {
        if let Ok(c_string) = CString::new(error_entry.0) {
            return c_string.into_raw();
        }
    }
    std::ptr::null()
}

#[no_mangle]
pub extern "C" fn sqlite3_column_type(stmt: *mut SQLite3PreparedStmt, col_index: i32) -> i32 {
    if stmt.is_null() {
        return SQLITE_NULL;
    }

    let stmt = unsafe { &mut *stmt };
    let result_rows = stmt.result_rows.lock().unwrap();
    let current_row = stmt.current_row.lock().unwrap();

    // Check if there's a current row
    if let Some(row_index) = *current_row {
        // Get the row at the current index
        if let Some(row) = result_rows.get(row_index) {
            // Get the value at the specified column index
            if let Some(value) = row.get(col_index as usize) {
                // Match the value type to determine SQLite type
                return match value {
                    Value::Integer(_) => SQLITE_INTEGER,
                    Value::Real(_) => SQLITE_FLOAT,
                    Value::Text(_) => SQLITE_TEXT,
                    Value::Null => SQLITE_NULL,
                };
            }
        }
    }

    SQLITE_NULL // Invalid column or no current row
}

#[no_mangle]
pub extern "C" fn sqlite3_column_bytes(stmt: *mut SQLite3PreparedStmt, col_index: i32) -> i32 {
    if stmt.is_null() {
        return 0;
    }

    let stmt = unsafe { &mut *stmt };
    let result_rows = stmt.result_rows.lock().unwrap();
    let current_row = stmt.current_row.lock().unwrap();

    // Check if there's a current row
    if let Some(row_index) = *current_row {
        // Get the row at the current index
        if let Some(row) = result_rows.get(row_index) {
            // Get the value at the specified column index
            if let Some(value) = row.get(col_index as usize) {
                // Calculate the byte length based on the value type
                return match value {
                    Value::Text(s) => s.len() as i32, // Length of the string in bytes
                    Value::Integer(_) => std::mem::size_of::<i64>() as i32, // Size of an integer
                    Value::Real(_) => std::mem::size_of::<f64>() as i32, // Size of a float
                    Value::Null => 0,                 // Null has no byte size
                };
            }
        }
    }

    0 // Invalid column or no current row
}

#[no_mangle]
pub extern "C" fn sqlite3_column_text(
    stmt: *mut SQLite3PreparedStmt,
    col_index: i32,
) -> *const c_char {
    if stmt.is_null() {
        return std::ptr::null();
    }

    let stmt = unsafe { &mut *stmt };
    let result_rows = stmt.result_rows.lock().unwrap();
    let current_row = stmt.current_row.lock().unwrap();

    // Check if there's a current row
    if let Some(row_index) = *current_row {
        if let Some(value) = result_rows
            .get(row_index)
            .and_then(|row| row.get(col_index as usize))
        {
            // Convert the value to a CString based on its type
            let text_representation = match value {
                Value::Text(s) => s.clone(),        // Use the text directly
                Value::Integer(i) => i.to_string(), // Convert integer to string
                Value::Real(f) => f.to_string(),    // Convert float to string
                Value::Null => "NULL".to_string(),  // Represent NULL as "NULL"
            };

            // Convert the string into a CString and return a raw pointer
            return CString::new(text_representation).unwrap().into_raw();
        }
    }

    std::ptr::null() // Invalid column or no current row
}

#[no_mangle]
pub extern "C" fn sqlite3_column_double(stmt: *mut SQLite3PreparedStmt, col_index: i32) -> f64 {
    if stmt.is_null() {
        return 0.0;
    }

    let stmt = unsafe { &mut *stmt };
    let result_rows = stmt.result_rows.lock().unwrap();
    let current_row = stmt.current_row.lock().unwrap();

    // Check if there's a current row
    if let Some(row_index) = *current_row {
        if let Some(value) = result_rows
            .get(row_index)
            .and_then(|row| row.get(col_index as usize))
        {
            // Match the value and extract it as f64
            return match value {
                Value::Real(f) => *f,                // Return the float directly
                Value::Integer(i) => *i as f64,      // Cast integer to float
                Value::Text(_) | Value::Null => 0.0, // Non-numeric or NULL
            };
        }
    }

    0.0
}

#[no_mangle]
pub extern "C" fn sqlite3_column_int64(stmt: *mut SQLite3PreparedStmt, col_index: i32) -> i64 {
    if stmt.is_null() {
        return 0;
    }

    let stmt = unsafe { &mut *stmt };
    let result_rows = stmt.result_rows.lock().unwrap();
    let current_row = stmt.current_row.lock().unwrap();

    // Check if there's a current row
    if let Some(row_index) = *current_row {
        // Get the row at the current index
        if let Some(row) = result_rows.get(row_index) {
            // Get the value at the specified column index
            if let Some(value) = row.get(col_index as usize) {
                // Match the value and extract it as i64
                return match value {
                    Value::Integer(i) => *i,           // Return the integer directly
                    Value::Real(f) => *f as i64,       // Cast float to integer
                    Value::Text(_) | Value::Null => 0, // Non-integer or NULL
                };
            }
        }
    }

    0 // Invalid column or no current row
}

#[no_mangle]
pub extern "C" fn sqlite3_column_name(
    stmt: *mut SQLite3PreparedStmt,
    col_index: i32,
) -> *const c_char {
    if stmt.is_null() {
        return std::ptr::null();
    }

    let stmt = unsafe { &mut *stmt };

    // Check if the column index is valid
    if col_index < 0 || col_index as usize >= stmt.column_names.len() {
        return std::ptr::null(); // Invalid column index
    }

    let column_name = &stmt.column_names[col_index as usize];

    // Convert column name to CString and return the pointer
    CString::new(column_name.as_str()).unwrap().into_raw()
}

#[no_mangle]
pub extern "C" fn sqlite3_column_table_name(
    stmt: *mut SQLite3PreparedStmt,
    col_index: i32,
) -> *const c_char {
    if stmt.is_null() {
        return std::ptr::null();
    }

    // Safety: Dereference the pointer to access the prepared statement
    let stmt = unsafe { &mut *stmt };

    // Validate the column index
    if col_index < 0 || col_index as usize >= stmt.column_names.len() {
        return std::ptr::null(); // Invalid column index
    }

    // Get the SQL query
    let sql = stmt.sql.as_str();

    // Use a regex to extract table names from the SQL query
    let table_regex = Regex::new(r"FROM\s+([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();

    // Match and capture the table name
    if let Some(captures) = table_regex.captures(sql) {
        if let Some(table_name) = captures.get(1) {
            // Return the table name as a CString
            match CString::new(table_name.as_str()) {
                Ok(c_string) => return c_string.into_raw(),
                Err(_) => return std::ptr::null(), // Handle CString creation error
            }
        }
    }

    std::ptr::null() // No table name found
}

#[no_mangle]
pub extern "C" fn sqlite3_errstr(errcode: c_int) -> *const c_char {
    let message = match errcode {
        SQLITE_OK => "Successful result",
        SQLITE_ERROR => "SQL error or missing database",
        SQLITE_MISUSE => "Library used incorrectly",
        SQLITE_RANGE => "2nd parameter to sqlite3_bind out of range",
        SQLITE_BUSY => "The database file is locked",
        SQLITE_CANTOPEN => "Either database does not exist or cannot be opened",
        _ => "Unknown error code",
    };

    CString::new(message).unwrap().into_raw()
}

#[no_mangle]
pub unsafe extern "C" fn sqlite3_exec(
    db: *mut SQLite3,
    sql: *const c_char,
    callback: SQLite3ExecCallback,
    arg: *mut c_void,
    errmsg: *mut *mut c_char,
) -> c_int {
    if !is_aligned(db) {
        return SQLITE_CANTOPEN;
    }

    let db = &mut *db;

    let sql = CStr::from_ptr(sql).to_string_lossy().to_string();

    if sql_is_pragma(&sql) {
        return SQLITE_OK;
    } else if sql_is_begin_transaction(&sql) {
        return execute_async_task(sqlite::begin_tnx_on_db(db, &sql));
    } else if sql_is_rollback(&sql) {
        return reset_txn_on_db(db);
    } else if sql_is_commit(&sql) {
        return execute_async_task(sqlite::commit_tnx_on_db(db, &sql));
    }

    execute_async_task(sqlite::handle_execute(db, &sql))
}

#[no_mangle]
pub extern "C" fn sqlite3_update_hook(
    db: *mut SQLite3,
    callback: Option<sqlite::SqliteHook>,
    user_data: *mut c_void,
) -> c_int {
    if !is_aligned(db) {
        return SQLITE_CANTOPEN;
    }

    let db = unsafe { &mut *db };

    db.register_hook(sqlite::SQLITE_UPDATE, callback, user_data)
}

#[no_mangle]
pub extern "C" fn sqlite3_commit_hook(
    db: *mut SQLite3,
    x_callback: Option<unsafe extern "C" fn(*mut c_void) -> c_int>, // int (*xCallback)(void*)
    p_arg: *mut c_void,                                             // void *pArg
) -> c_int {
    if !is_aligned(db) {
        return SQLITE_CANTOPEN;
    }

    SQLITE_OK
}

#[no_mangle]
pub extern "C" fn sqlite3_rollback_hook(
    db: *mut SQLite3,
    x_callback: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
    p_arg: *mut c_void,
) -> c_int {
    if !is_aligned(db) {
        return SQLITE_CANTOPEN;
    }

    SQLITE_OK
}

#[no_mangle]
pub extern "C" fn sqlite3_get_autocommit(db: *mut SQLite3) -> c_int {
    if !is_aligned(db) {
        return 1;
    }

    let db = unsafe { &*db };

    if db.has_began_transaction() {
        0 // Transaction is active
    } else {
        1 // Autocommit mode
    }
}

#[no_mangle]
pub extern "C" fn sqlite3_create_function_v2(
    _db: *mut c_void,
    _zFunctionName: *const c_char,
    _nArg: c_int,
    _eTextRep: c_int,
    _pApp: *mut c_void,
    _xFunc: Option<extern "C" fn(*mut c_void, c_int, *mut *mut c_void)>,
    _xStep: Option<extern "C" fn(*mut c_void, c_int, *mut *mut c_void)>,
    _xFinal: Option<extern "C" fn(*mut c_void)>,
    _xDestroy: Option<extern "C" fn(*mut c_void)>,
) -> c_int {
    if cfg!(debug_assertions) {
        eprintln!(
            "Not Yet Supported: sqlite3_create_function_v2 : {:?}",
            unsafe { CStr::from_ptr(_zFunctionName) }
        );
    }
    SQLITE_OK
}

#[no_mangle]
pub extern "C" fn sqlite3_stmt_isexplain(stmt: *mut SQLite3PreparedStmt) -> c_int {
    if !is_aligned(stmt) {
        return SQLITE_OK;
    }

    let stmt_ref = unsafe { &*stmt };
    let sql_trimmed = stmt_ref.sql.trim_start().to_uppercase();

    if sql_trimmed.starts_with("EXPLAIN QUERY PLAN") {
        2 // EXPLAIN QUERY PLAN
    } else if sql_trimmed.starts_with("EXPLAIN") {
        1 // EXPLAIN (no QUERY PLAN)
    } else {
        SQLITE_OK
    }
}

#[no_mangle]
pub extern "C" fn sqlite3_compileoption_used(opt_name: *const c_char) -> c_int {
    if opt_name.is_null() {
        return 0;
    }
    let opt = unsafe { CStr::from_ptr(opt_name) };
    let opt_str = match opt.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };
    // SQLite C API allows callers to omit the "SQLITE_" prefix
    let opt_str = opt_str.strip_prefix("SQLITE_").unwrap_or(opt_str);

    match opt_str {
        // sqlite3_column_table_name is already implemented in this proxy
        "ENABLE_COLUMN_METADATA" => 1,
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn sqlite3_compileoption_get(n: c_int) -> *const c_char {
    match n {
        0 => b"ENABLE_COLUMN_METADATA\0".as_ptr() as *const c_char,
        _ => std::ptr::null(),
    }
}