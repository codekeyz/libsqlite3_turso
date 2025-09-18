use std::{collections::HashMap, ffi::c_int, sync::OnceLock};

use regex::Regex;
use tokio::runtime::{self, Runtime};

use crate::{
    sqlite::{push_error, SQLite3, SqliteError, Value, SQLITE_ERROR},
    transport::{QueryResult, RemoteSQLiteResult, RemoteSqliteResponse},
};

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

pub fn get_tokio() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        runtime::Builder::new_multi_thread()
            .worker_threads(num_cpus::get())
            .enable_all()
            .build()
            .expect("Failed to create Tokio runtime")
    })
}

// Function to count parameters in the SQL string
pub fn count_parameters(sql: &str) -> c_int {
    let re = Regex::new(
        r"(?x)
        (?:
            \?            # anonymous
            \d*           # or numbered
          |
            [:@$]         # named parameters
            [a-zA-Z_]\w*
        )
    ",
    )
    .unwrap();

    re.find_iter(&sql).count() as c_int
}

pub fn execute_async_task<F, R>(task: F) -> c_int
where
    F: std::future::Future<Output = Result<R, SqliteError>>,
    R: Into<c_int>,
{
    let runtime = get_tokio();

    match runtime.block_on(task) {
        Ok(result) => result.into(),
        Err(err) => {
            unsafe { push_error((format!("{}", err), err.code)) };
            SQLITE_ERROR
        }
    }
}

#[inline]
pub fn sql_is_begin_transaction(sql: &String) -> bool {
    sql.starts_with("BEGIN")
}

#[inline]
pub fn sql_is_pragma(sql: &String) -> bool {
    sql.starts_with("PRAGMA")
}

#[inline]
pub fn sql_is_rollback(sql: &String) -> bool {
    sql.starts_with("ROLLBACK")
}

#[inline]
pub fn sql_is_commit(sql: &String) -> bool {
    sql.starts_with("COMMIT")
}

#[inline]
pub fn is_aligned<T>(ptr: *const T) -> bool {
    !ptr.is_null() && (ptr as usize) % std::mem::align_of::<T>() == 0
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
) -> Result<&'a QueryResult, SqliteError> {
    let mut baton = db.transaction_baton.lock().unwrap();

    if let Some(new_baton) = &result.baton {
        baton.replace(new_baton.into());
    }

    let first_execution_result = match result.results.get(0) {
        Some(inner) => match &inner.response {
            RemoteSQLiteResult::Error { message, code } => {
                return Err(SqliteError::new(
                    format!("Remote SQLite error (code {}): {}", code, message),
                    Some(SQLITE_ERROR),
                ));
            }
            RemoteSQLiteResult::Execute { result } => Ok(result),
            RemoteSQLiteResult::Close => Err::<&'a QueryResult, SqliteError>(SqliteError::new(
                "Remote SQLite closed the connection unexpectedly",
                None,
            )),
        },
        None => Err::<&'a QueryResult, SqliteError>(SqliteError::new(
            "No results returned from remote SQLite",
            None,
        )),
    }?;

    if let Some(last_insert_rowid) = &first_execution_result.last_insert_rowid {
        let mut last_insert_rowid_lock = db.last_insert_rowid.lock().unwrap();
        *last_insert_rowid_lock = Some(last_insert_rowid.parse::<i64>().unwrap_or(0));
    }

    if let Some(rows_written) = &first_execution_result.rows_written {
        let mut rows_written_lock = db.rows_written.lock().unwrap();
        *rows_written_lock = Some(*rows_written);
    }

    Ok(first_execution_result)
}
