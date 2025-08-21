use std::{future::Future, pin::Pin};

use crate::transport::TursoConfig;

pub trait DbAuthStrategy {
    fn resolve<'a>(
        &'a self,
        db_name: &'a str,
        client: &'a reqwest::Client,
    ) -> Pin<Box<dyn Future<Output = Result<TursoConfig, Box<dyn std::error::Error>>> + Send + 'a>>;
}

pub struct GlobeStrategy;

impl DbAuthStrategy for GlobeStrategy {
    fn resolve<'a>(
        &'a self,
        db_name: &'a str,
        client: &'a reqwest::Client,
    ) -> Pin<Box<dyn Future<Output = Result<TursoConfig, Box<dyn std::error::Error>>> + Send + 'a>>
    {
        Box::pin(async move {
            let globe_auth_api = std::env::var("GLOBE_DS_API")
                .map_err(|_| "GLOBE_DS_API environment variable not set")?;

            let clean_db_name = db_name.split('.').next().unwrap_or(db_name);

            let response = client
                .get(format!("{}/db/{}/get_auth", globe_auth_api, clean_db_name))
                .send()
                .await
                .map_err(|_| "Failed to fetch auth credentials for database")?;

            let status_code = response.status();
            if !status_code.is_success() {
                let error_message = response.text().await?;
                if cfg!(debug_assertions) {
                    eprintln!("Error: {}", error_message);
                }

                return Err(format!(
                    "Failed to authenticate database. Http Status Code: {}, Error: {}",
                    status_code, error_message
                )
                .into());
            }

            let json = response.json().await?;
            let config = serde_json::from_value(json)?;
            Ok(config)
        })
    }
}

pub struct EnvVarStrategy;

impl DbAuthStrategy for EnvVarStrategy {
    fn resolve<'a>(
        &'a self,
        _: &'a str,
        _client: &'a reqwest::Client,
    ) -> Pin<Box<dyn Future<Output = Result<TursoConfig, Box<dyn std::error::Error>>> + Send + 'a>>
    {
        Box::pin(async move {
            let url = std::env::var("TURSO_DB_URL")
                .map_err(|_| "TURSO_DB_URL environment variable not set")?;
            let token = std::env::var("TURSO_DB_TOKEN")
                .map_err(|_| "TURSO_DB_TOKEN environment variable not set")?;

            Ok(TursoConfig {
                db_url: url,
                db_token: token,
            })
        })
    }
}
