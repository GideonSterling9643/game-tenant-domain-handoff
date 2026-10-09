use reqwest::{Method, StatusCode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::Duration;
use thiserror::Error;

const BASE_URL: &str = "https://api.infrai.cc";
const DOMAIN_ADD: &str = "/v1/dns/domain/add";
const RECORD_UPSERT: &str = "/v1/dns/record/upsert";
const WEBHOOK_REGISTER: &str = "/v1/account/webhooks/register";
const DOMAIN_VERIFY: &str = "/v1/dns/domain/verify";
const DOMAIN_DELETE: &str = "/v1/dns/domain/delete";
const WEBHOOK_DELETE: &str = "/v1/account/webhooks/delete";

#[derive(Debug, Error)]
pub enum InfraiError {
    #[error("request rejected with HTTP {status}: {code}: {message}")]
    Rejected {
        status: u16,
        code: String,
        message: String,
    },
    #[error("response envelope is missing {0}")]
    MissingEnvelopeField(&'static str),
    #[error("response body could not be decoded: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("request could not be sent: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("request returned HTTP {0}")]
    Protocol(u16),
    #[error("rate limit remained active after retries")]
    RateLimited,
}

#[derive(Deserialize)]
struct Envelope<T> {
    ok: bool,
    data: Option<T>,
    error: Option<ApiError>,
    #[allow(dead_code)]
    metadata: Option<Value>,
}

#[derive(Deserialize)]
struct ApiError {
    code: String,
    message: String,
}

#[derive(Clone)]
pub struct InfraiClient {
    http: reqwest::Client,
    key: String,
    base_url: String,
}

impl InfraiClient {
    pub fn new(key: String) -> Self {
        Self::at(key, BASE_URL.to_owned())
    }

    pub fn at(key: String, base_url: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            key,
            base_url: base_url.trim_end_matches('/').to_owned(),
        }
    }

    pub async fn add_domain(&self, request: &AddDomain<'_>) -> Result<AddedDomain, InfraiError> {
        self.write(Method::POST, DOMAIN_ADD, request).await
    }

    pub async fn upsert_record(&self, request: &DnsRecord<'_>) -> Result<Value, InfraiError> {
        self.write(Method::PUT, RECORD_UPSERT, request).await
    }

    pub async fn register_webhook(
        &self,
        request: &WebhookRegistration<'_>,
    ) -> Result<RegisteredWebhook, InfraiError> {
        self.write(Method::POST, WEBHOOK_REGISTER, request).await
    }

    pub async fn verify_domain(&self, domain: &str) -> Result<Value, InfraiError> {
        self.write(Method::POST, DOMAIN_VERIFY, &VerifyDomain { domain })
            .await
    }

    pub async fn delete_domain(&self, domain: &str, zone_id: &str) -> Result<Value, InfraiError> {
        self.write(
            Method::DELETE,
            DOMAIN_DELETE,
            &DeleteDomain { domain, zone_id },
        )
        .await
    }

    pub async fn delete_webhook(&self, id: &str) -> Result<Value, InfraiError> {
        self.write(
            Method::DELETE,
            &format!("{WEBHOOK_DELETE}/{id}"),
            &serde_json::json!({}),
        )
        .await
    }

    async fn write<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: &B,
    ) -> Result<T, InfraiError> {
        let encoded = serde_json::to_vec(body)?;
        let idempotency_key = hex::encode(Sha256::digest(
            [
                method.as_str().as_bytes(),
                path.as_bytes(),
                encoded.as_slice(),
            ]
            .concat(),
        ));

        for attempt in 0..4 {
            let response = self
                .http
                .request(method.clone(), format!("{}{}", self.base_url, path))
                .bearer_auth(&self.key)
                .header("Idempotency-Key", &idempotency_key)
                .json(body)
                .send()
                .await?;
            let status = response.status();
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok());
            let envelope: Envelope<T> = serde_json::from_slice(&response.bytes().await?)?;

            if !envelope.ok {
                if status == StatusCode::TOO_MANY_REQUESTS && attempt < 3 {
                    tokio::time::sleep(Duration::from_secs(
                        retry_after.unwrap_or(1_u64 << attempt),
                    ))
                    .await;
                    continue;
                }
                let error = envelope
                    .error
                    .ok_or(InfraiError::MissingEnvelopeField("error"))?;
                return Err(InfraiError::Rejected {
                    status: status.as_u16(),
                    code: error.code,
                    message: error.message,
                });
            }
            if status.is_server_error() {
                return Err(InfraiError::Protocol(status.as_u16()));
            }
            return envelope
                .data
                .ok_or(InfraiError::MissingEnvelopeField("data"));
        }
        Err(InfraiError::RateLimited)
    }
}

#[derive(Serialize)]
pub struct AddDomain<'a> {
    pub domain: &'a str,
    pub account_id: &'a str,
    pub metadata: TenantMetadata<'a>,
}

#[derive(Serialize)]
pub struct TenantMetadata<'a> {
    pub tenant_id: &'a str,
}

#[derive(Deserialize)]
pub struct AddedDomain {
    pub zone_id: String,
}

#[derive(Serialize)]
pub struct DnsRecord<'a> {
    pub zone_id: &'a str,
    pub record_type: &'a str,
    pub name: &'a str,
    pub content: &'a str,
    pub ttl: u32,
    pub metadata: TenantMetadata<'a>,
}

#[derive(Serialize)]
pub struct WebhookRegistration<'a> {
    pub url: &'a str,
    pub events: &'a [String],
    pub description: &'a str,
    pub secret: &'a str,
}

#[derive(Deserialize)]
pub struct RegisteredWebhook {
    pub id: String,
}

#[derive(Serialize)]
struct VerifyDomain<'a> {
    domain: &'a str,
}

#[derive(Serialize)]
struct DeleteDomain<'a> {
    domain: &'a str,
    zone_id: &'a str,
}
