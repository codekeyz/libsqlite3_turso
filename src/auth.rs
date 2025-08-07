use std::{future::Future, pin::Pin};

use crate::utils::TursoConfig;

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
            let globe_auth_api = std::env::var("GLOBE_DS_API")?;

            let request_body = serde_json::json!({ "db_name": db_name });

            let response = client
                .post(format!("{}/db/auth", globe_auth_api))
                .body(request_body.to_string())
                .send()
                .await
                .map_err(|_| "Failed to fetch auth credentials for database")?;

            if !response.status().is_success() {
                return Err(format!("Failed to get Auth Token: {}", response.status()).into());
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
            let url = std::env::var("TURSO_DB_URL")?;
            let token = std::env::var("TURSO_DB_TOKEN")?;

            Ok(TursoConfig {
                db_url: url,
                db_token: token,
            })
        })
    }
}
