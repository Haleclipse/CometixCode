//! CLIProxyAPI channel implementation.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use futures::future::BoxFuture;
use serde_json::Value;
use url::Url;

use super::traits::ProxyChannel;
use super::types::{DiscoveredModel, Endpoints};

pub const DEFAULT_CPA_BASE_URL: &str = "http://127.0.0.1:8317";
const CLIENT_VERSION_PARAM: &str = "cometix";
const REQUEST_TIMEOUT_SECS: u64 = 30;

pub struct CliProxyApiChannel;

impl CliProxyApiChannel {
    pub fn normalize_endpoints_impl(input: &str) -> Result<Endpoints> {
        let raw = input.trim();
        if raw.is_empty() {
            return Err(anyhow!("Base URL cannot be empty"));
        }

        let with_scheme = if !raw.starts_with("http://") && !raw.starts_with("https://") {
            format!("http://{raw}")
        } else {
            raw.to_string()
        };

        let parsed = Url::parse(&with_scheme).context("Failed to parse base URL")?;
        let origin = parsed.origin().ascii_serialization();

        let mut path = parsed.path().trim_end_matches('/').to_string();
        if path.ends_with("/v1") {
            path = path[..path.len() - 3].trim_end_matches('/').to_string();
        } else if path.ends_with("/backend-api") {
            path = path[..path.len() - 12].trim_end_matches('/').to_string();
        }

        let inference_base_url = if path.is_empty() {
            origin.clone()
        } else {
            format!("{origin}{path}")
        };

        let models_path = if path.is_empty() {
            "/v1/models".to_string()
        } else {
            format!("{path}/v1/models")
        };

        let models_url = format!("{origin}{models_path}?client_version={CLIENT_VERSION_PARAM}");

        Ok(Endpoints {
            inference_base_url,
            models_url,
        })
    }

    pub async fn fetch_models_impl(
        ep: &Endpoints,
        api_key: &str,
    ) -> Result<Vec<DiscoveredModel>> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .context("Failed to create HTTP client")?;

        let request = client
            .get(&ep.models_url)
            .header("Accept", "application/json")
            .header("Authorization", format!("Bearer {api_key}"))
            .header("x-api-key", api_key);

        let response = request
            .send()
            .await
            .with_context(|| format!("Failed to reach models endpoint at {}", ep.models_url))?;

        let status = response.status();
        if !status.is_success() {
            let err_text = response.text().await.unwrap_or_default();
            let snippet = if err_text.len() > 200 {
                format!("{}...", &err_text[..200])
            } else {
                err_text
            };
            return Err(anyhow!(
                "Models request returned status {}: {}",
                status,
                snippet
            ));
        }

        let payload: Value = response
            .json()
            .await
            .context("Failed to parse JSON response from models endpoint")?;

        Ok(parse_models_json(&payload))
    }
}

impl ProxyChannel for CliProxyApiChannel {
    fn id(&self) -> &'static str {
        "cpa"
    }

    fn display_name(&self) -> &'static str {
        "CLIProxyAPI"
    }

    fn default_base_url(&self) -> &'static str {
        DEFAULT_CPA_BASE_URL
    }

    fn normalize_endpoints(&self, input: &str) -> Result<Endpoints> {
        Self::normalize_endpoints_impl(input)
    }

    fn fetch_models<'a>(
        &'a self,
        ep: &'a Endpoints,
        api_key: &'a str,
    ) -> BoxFuture<'a, Result<Vec<DiscoveredModel>>> {
        Box::pin(Self::fetch_models_impl(ep, api_key))
    }
}

/// Robust parser for diverse model response formats.
pub fn parse_models_json(payload: &Value) -> Vec<DiscoveredModel> {
    let items = if let Some(arr) = payload.as_array() {
        arr.as_slice()
    } else if let Some(arr) = payload.get("data").and_then(Value::as_array) {
        arr.as_slice()
    } else if let Some(arr) = payload.get("models").and_then(Value::as_array) {
        arr.as_slice()
    } else {
        &[]
    };

    let mut models = Vec::new();
    for item in items {
        if let Some(visibility) = item.get("visibility").and_then(Value::as_str) {
            if visibility.eq_ignore_ascii_case("hide") {
                continue;
            }
        }

        let id = item
            .get("id")
            .and_then(Value::as_str)
            .or_else(|| item.get("slug").and_then(Value::as_str));

        let Some(id) = id else { continue };
        let id = id.trim();
        if id.is_empty() {
            continue;
        }

        let name = item
            .get("display_name")
            .and_then(Value::as_str)
            .or_else(|| item.get("name").and_then(Value::as_str))
            .unwrap_or(id)
            .trim();

        let description = item
            .get("description")
            .and_then(Value::as_str)
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty());

        models.push(DiscoveredModel {
            id: id.to_string(),
            name: if name.is_empty() {
                id.to_string()
            } else {
                name.to_string()
            },
            description,
        });
    }

    models
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_endpoints() {
        let channel = CliProxyApiChannel;

        // Bare host:port
        let ep = channel.normalize_endpoints("127.0.0.1:8317").unwrap();
        assert_eq!(ep.inference_base_url, "http://127.0.0.1:8317");
        assert_eq!(
            ep.models_url,
            "http://127.0.0.1:8317/v1/models?client_version=cometix"
        );

        // With trailing slash
        let ep = channel.normalize_endpoints("http://127.0.0.1:8317/").unwrap();
        assert_eq!(ep.inference_base_url, "http://127.0.0.1:8317");
        assert_eq!(
            ep.models_url,
            "http://127.0.0.1:8317/v1/models?client_version=cometix"
        );

        // With /v1 suffix
        let ep = channel.normalize_endpoints("http://127.0.0.1:8317/v1").unwrap();
        assert_eq!(ep.inference_base_url, "http://127.0.0.1:8317");
        assert_eq!(
            ep.models_url,
            "http://127.0.0.1:8317/v1/models?client_version=cometix"
        );

        // With /backend-api suffix
        let ep = channel
            .normalize_endpoints("http://127.0.0.1:8317/backend-api")
            .unwrap();
        assert_eq!(ep.inference_base_url, "http://127.0.0.1:8317");
        assert_eq!(
            ep.models_url,
            "http://127.0.0.1:8317/v1/models?client_version=cometix"
        );

        // Subpath
        let ep = channel
            .normalize_endpoints("https://proxy.example.com/prefix/v1")
            .unwrap();
        assert_eq!(ep.inference_base_url, "https://proxy.example.com/prefix");
        assert_eq!(
            ep.models_url,
            "https://proxy.example.com/prefix/v1/models?client_version=cometix"
        );
    }

    #[test]
    fn test_parse_models_json() {
        let json = serde_json::json!({
            "data": [
                {
                    "id": "claude-3-7-sonnet",
                    "display_name": "Claude 3.7 Sonnet",
                    "description": "Smart model"
                },
                {
                    "slug": "gemini-2.5-pro",
                    "name": "Gemini 2.5 Pro"
                },
                {
                    "id": "hidden-model",
                    "visibility": "hide"
                }
            ]
        });

        let models = parse_models_json(&json);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "claude-3-7-sonnet");
        assert_eq!(models[0].name, "Claude 3.7 Sonnet");
        assert_eq!(models[0].description.as_deref(), Some("Smart model"));
        assert_eq!(models[1].id, "gemini-2.5-pro");
        assert_eq!(models[1].name, "Gemini 2.5 Pro");
        assert_eq!(models[1].description, None);
    }
}
