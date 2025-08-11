use std::{ffi::c_int, sync::OnceLock};

use regex::Regex;
use serde::Deserialize;
use tokio::runtime::{self, Runtime};

use crate::sqlite::{push_error, SQLite3, SqliteError, SQLITE_ERROR};

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

pub fn execute_async_task<F, R>(db: *mut SQLite3, task: F) -> c_int
where
    F: std::future::Future<Output = Result<R, SqliteError>>,
    R: Into<c_int>,
{
    let runtime = get_tokio();

    match runtime.block_on(task) {
        Ok(result) => result.into(),
        Err(err) => {
            push_error(db, (format!("{}", err), err.code));
            SQLITE_ERROR
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct TursoConfig {
    pub db_url: String,
    pub db_token: String,
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
