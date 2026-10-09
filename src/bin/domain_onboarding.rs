use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use game_domain_handoff::{
    game_tenant::{GameTenant, OnboardingError, TenantOnboarding},
    infrai_client::{InfraiClient, InfraiError},
};
use std::{env, net::SocketAddr};
use thiserror::Error;

#[derive(Debug, Error)]
enum ServiceError {
    #[error("missing environment variable {0}")]
    MissingEnv(&'static str),
    #[error("invalid LISTEN_ADDR")]
    InvalidAddress,
    #[error(transparent)]
    Onboarding(#[from] OnboardingError),
    #[error("listener error: {0}")]
    Listener(#[from] std::io::Error),
}

#[tokio::main]
async fn main() -> Result<(), ServiceError> {
    let key = required("INFRAI_API_KEY")?;
    let secret = required("INFRAI_WEBHOOK_SECRET")?;
    // The same INFRAI_API_KEY and base URL authorize DNS writes and webhook registration.
    let onboarding = TenantOnboarding::new(InfraiClient::new(key), secret);
    let tenant = GameTenant {
        tenant_id: required("GAME_TENANT_ID")?,
        domain: required("GAME_DOMAIN")?,
        edge_target: required("GAME_EDGE_TARGET")?,
        webhook_url: required("PUBLIC_WEBHOOK_URL")?,
        webhook_events: required("INFRAI_WEBHOOK_EVENTS")?
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect(),
        asset_namespace: required("ASSET_NAMESPACE")?,
        live_event_channel: required("LIVE_EVENT_CHANNEL")?,
        moderation_queue: required("MODERATION_QUEUE")?,
    };
    let handoff = onboarding.begin(&tenant).await?;
    println!("{}", serde_json::to_string_pretty(&handoff).unwrap());

    let address: SocketAddr = env::var("LISTEN_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:3000".to_owned())
        .parse()
        .map_err(|_| ServiceError::InvalidAddress)?;
    let app = Router::new()
        .route("/domain-verification", post(domain_verification))
        .with_state(onboarding);
    let listener = tokio::net::TcpListener::bind(address).await?;
    println!("listening on http://{address}/domain-verification");
    axum::serve(listener, app).await?;
    Ok(())
}

fn required(name: &'static str) -> Result<String, ServiceError> {
    env::var(name).map_err(|_| ServiceError::MissingEnv(name))
}

async fn domain_verification(
    State(onboarding): State<TenantOnboarding>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let signature = headers
        .get("x-infrai-signature")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    match onboarding.receive_verification(signature, &body).await {
        Ok(state) => (StatusCode::OK, Json(serde_json::json!({ "state": state }))),
        Err(error) => {
            let status = match &error {
                OnboardingError::InvalidSignature | OnboardingError::InvalidPayload(_) => {
                    StatusCode::BAD_REQUEST
                }
                OnboardingError::Infrai(InfraiError::Rejected { status, .. }) => {
                    StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_REQUEST)
                }
                OnboardingError::Infrai(_) | OnboardingError::CleanupFailed { .. } => {
                    StatusCode::BAD_GATEWAY
                }
            };
            (status, Json(serde_json::json!({ "error": error.to_string() })))
        }
    }
}
