use regex::Regex;
use std::{
    collections::HashMap,
    ffi::{c_int, c_uint, c_void, CStr, CString},
    os::raw::c_char,
    slice,
    sync::Mutex,
};

use sqlite::{
    push_error, reset_txn_on_db, ExecutionState, SQLite3, SQLite3PreparedStmt, Value, SQLITE_BUSY,
    SQLITE_DONE, SQLITE_ERROR, SQLITE_FLOAT, SQLITE_INTEGER, SQLITE_MISUSE, SQLITE_NULL, SQLITE_OK,
    SQLITE_RANGE, SQLITE_TEXT,
};
use utils::execute_async_task;

use crate::{
    auth::{DbAuthStrategy, GlobeStrategy},
    utils::{
        count_parameters, extract_column_names, get_tokio, sql_is_begin_transaction, sql_is_commit,
        sql_is_pragma, sql_is_rollback,
    },
};

mod auth;
mod proxy;
mod sqlite;
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

    let filename = CStr::from_ptr(filename).to_str().unwrap();
    if filename.contains(":memory") {
        eprintln!("LibSqlite3_Turso Error: Memory store is not supported at runtime");
        return SQLITE_MISUSE;
    }

    let reqwest_client = reqwest::Client::builder()
        .user_agent("libsqlite3_turso/1.0.0")
        .build()
        .unwrap();

    let auth_strategy = Box::new(GlobeStrategy);
    let turso_config = get_tokio().block_on(auth_strategy.resolve(filename, &reqwest_client));
    if turso_config.is_err() {
        eprintln!("LibSqlite3_Turso Error: {}", turso_config.unwrap_err());
        return SQLITE_ERROR;
    }

    let mock_db = Box::into_raw(Box::new(SQLite3 {
        client: reqwest_client,
        error_stack: Mutex::new(vec![]),
        transaction_baton: Mutex::new(None),
        last_insert_rowid: Mutex::new(None),
        delete_hook: Mutex::new(None),
        insert_hook: Mutex::new(None),
        update_hook: Mutex::new(None),
        turso_config: turso_config.unwrap(),
    }));

    *db = mock_db;

    SQLITE_OK
}

#[no_mangle]
pub extern "C" fn sqlite3_extended_result_codes(db: *mut SQLite3, _onoff: i32) -> i32 {
    if db.is_null() {
        return SQLITE_MISUSE;
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
    if pp_stmt.is_null() {
        return SQLITE_ERROR;
    }

    let db = &mut *_db;

    if prep_flag != 0 {
        push_error(
            db,
            (
                "Persisted prepared statements not supported yet.".to_string(),
                SQLITE_MISUSE,
            ),
        );
        return SQLITE_MISUSE;
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
    let column_names = extract_column_names(&sql);

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
        column_names,
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

    let sql = stmt.sql.to_uppercase();
    if sql.starts_with("SELECT") {
        return execute_async_task(stmt.db, sqlite::handle_select(stmt));
    } else if sql_is_begin_transaction(&sql) {
        return execute_async_task(stmt.db, sqlite::begin_tnx_on_db(stmt.db));
    } else if sql_is_commit(&sql) {
        return execute_async_task(stmt.db, sqlite::commit_tnx_on_db(stmt.db));
    }

    execute_async_task(stmt.db, sqlite::execute_statement(stmt))
}

#[no_mangle]
pub extern "C" fn sqlite3_column_count(stmt: *mut SQLite3PreparedStmt) -> i32 {
    if stmt.is_null() {
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

    SQLITE_OK // Indicate successful reset
}

#[no_mangle]
pub unsafe extern "C" fn sqlite3_close_v2(db: *mut SQLite3) -> c_int {
    if db.is_null() {
        return SQLITE_OK;
    }

    let db = unsafe { &mut *db };

    reset_txn_on_db(db);

    drop(Box::from_raw(db));

    SQLITE_OK
}

#[no_mangle]
pub extern "C" fn sqlite3_extended_errcode(db: *mut SQLite3) -> c_int {
    if db.is_null() {
        return SQLITE_OK;
    }

    let db = unsafe { &mut *db };

    let error_stack = db.error_stack.lock().unwrap();
    if !error_stack.is_empty() {
        return error_stack[0].1;
    }

    SQLITE_OK
}

#[no_mangle]
pub extern "C" fn sqlite3_errmsg(db: *mut SQLite3) -> *const c_char {
    if db.is_null() {
        return b"Invalid DB pointer\0".as_ptr() as *const c_char;
    }

    let db = unsafe { &mut *db };

    if let Some(error_entry) = sqlite::get_latest_error(db) {
        match CString::new(error_entry.0) {
            Ok(c_string) => c_string.as_ptr(),
            Err(_) => std::ptr::null(),
        }
    } else {
        b"No error\0".as_ptr() as *const c_char
    }
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
        _ => "Unknown error code",
    };

    CString::new(message).unwrap().into_raw()
}

#[no_mangle]
pub unsafe extern "C" fn sqlite3_exec(
    db: *mut SQLite3,
    sql: *const c_char,
    _: Option<extern "C" fn(*mut std::ffi::c_void, i32, *const *const i8, *const *const i8) -> i32>,
    _: *mut std::ffi::c_void,
    _: *mut *mut i8,
) -> c_int {
    if db.is_null() || sql.is_null() {
        return SQLITE_MISUSE;
    }

    let db = &mut *db;

    let sql = CStr::from_ptr(sql).to_string_lossy().to_string();

    if sql_is_pragma(&sql) {
        return SQLITE_OK;
    } else if sql_is_begin_transaction(&sql) {
        return execute_async_task(db, sqlite::begin_tnx_on_db(db));
    } else if sql_is_rollback(&sql) {
        return reset_txn_on_db(db);
    } else if sql_is_commit(&sql) {
        return execute_async_task(db, sqlite::commit_tnx_on_db(db));
    }

    execute_async_task(db, sqlite::handle_execute(db, &sql))
}

#[no_mangle]
pub extern "C" fn sqlite3_update_hook(
    db: *mut SQLite3,
    callback: Option<sqlite::SqliteHook>,
    user_data: *mut c_void,
) -> c_int {
    if db.is_null() {
        return SQLITE_MISUSE;
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
    if db.is_null() {
        return SQLITE_MISUSE;
    }

    SQLITE_OK
}

#[no_mangle]
pub extern "C" fn sqlite3_rollback_hook(
    db: *mut SQLite3,
    x_callback: Option<unsafe extern "C" fn(*mut c_void) -> c_int>, // int (*xCallback)(void*)
    p_arg: *mut c_void,                                             // void *pArg
) {
    if db.is_null() {
        return;
    }

    let db = unsafe { &mut *db }; // Safely dereference the raw pointer
}

#[no_mangle]
pub extern "C" fn sqlite3_get_autocommit(db: *mut SQLite3) -> c_int {
    if db.is_null() {
        return 1;
    }

    let db = unsafe { &*db };

    if db.transaction_active() {
        0 // Transaction is active
    } else {
        1 // Autocommit mode
    }
}
