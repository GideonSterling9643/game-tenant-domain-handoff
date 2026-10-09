use crate::infrai_client::{
    AddDomain, DnsRecord, InfraiClient, InfraiError, TenantMetadata, WebhookRegistration,
};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::{collections::HashMap, sync::Arc};
use thiserror::Error;
use tokio::sync::RwLock;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Deserialize)]
pub struct GameTenant {
    pub tenant_id: String,
    pub domain: String,
    pub edge_target: String,
    pub webhook_url: String,
    pub webhook_events: Vec<String>,
    pub asset_namespace: String,
    pub live_event_channel: String,
    pub moderation_queue: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DomainState {
    AwaitingDns,
    Ready,
}

#[derive(Debug, Serialize)]
pub struct TenantHandoff {
    pub tenant_id: String,
    pub domain: String,
    pub zone_id: String,
    pub webhook_id: String,
    pub asset_namespace: String,
    pub live_event_channel: String,
    pub moderation_queue: String,
    pub state: DomainState,
}

#[derive(Debug, Error)]
pub enum OnboardingError {
    #[error(transparent)]
    Infrai(#[from] InfraiError),
    #[error("webhook signature is invalid")]
    InvalidSignature,
    #[error("webhook payload is invalid: {0}")]
    InvalidPayload(#[from] serde_json::Error),
    #[error("onboarding failed ({onboarding}); cleanup also failed ({cleanup})")]
    CleanupFailed { onboarding: String, cleanup: String },
}

#[derive(Deserialize)]
struct VerificationEvent {
    domain: String,
    verified: bool,
}

#[derive(Clone)]
pub struct TenantOnboarding {
    client: InfraiClient,
    secret: Arc<String>,
    states: Arc<RwLock<HashMap<String, DomainState>>>,
}

impl TenantOnboarding {
    pub fn new(client: InfraiClient, secret: String) -> Self {
        Self {
            client,
            secret: Arc::new(secret),
            states: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn begin(&self, tenant: &GameTenant) -> Result<TenantHandoff, OnboardingError> {
        let added = self
            .client
            .add_domain(&AddDomain {
                domain: &tenant.domain,
                account_id: &tenant.tenant_id,
                metadata: TenantMetadata {
                    tenant_id: &tenant.tenant_id,
                },
            })
            .await?;

        if let Err(error) = self
            .client
            .upsert_record(&DnsRecord {
                zone_id: &added.zone_id,
                record_type: "CNAME",
                name: &tenant.domain,
                content: &tenant.edge_target,
                ttl: 300,
                metadata: TenantMetadata {
                    tenant_id: &tenant.tenant_id,
                },
            })
            .await
        {
            return Err(self
                .rollback_domain(&tenant.domain, &added.zone_id, error)
                .await);
        }

        let webhook = match self
            .client
            .register_webhook(&WebhookRegistration {
                url: &tenant.webhook_url,
                events: &tenant.webhook_events,
                description: "Game tenant domain verification",
                secret: &self.secret,
            })
            .await
        {
            Ok(webhook) => webhook,
            Err(error) => {
                return Err(self
                    .rollback_domain(&tenant.domain, &added.zone_id, error)
                    .await)
            }
        };
        if let Err(error) = self.client.verify_domain(&tenant.domain).await {
            let webhook_cleanup = self.client.delete_webhook(&webhook.id).await;
            let domain_cleanup = self
                .client
                .delete_domain(&tenant.domain, &added.zone_id)
                .await;
            if let Err(cleanup) = webhook_cleanup.and(domain_cleanup) {
                return Err(OnboardingError::CleanupFailed {
                    onboarding: error.to_string(),
                    cleanup: cleanup.to_string(),
                });
            }
            return Err(error.into());
        }

        self.states
            .write()
            .await
            .insert(tenant.domain.clone(), DomainState::AwaitingDns);

        Ok(TenantHandoff {
            tenant_id: tenant.tenant_id.clone(),
            domain: tenant.domain.clone(),
            zone_id: added.zone_id,
            webhook_id: webhook.id,
            asset_namespace: tenant.asset_namespace.clone(),
            live_event_channel: tenant.live_event_channel.clone(),
            moderation_queue: tenant.moderation_queue.clone(),
            state: DomainState::AwaitingDns,
        })
    }

    async fn rollback_domain(
        &self,
        domain: &str,
        zone_id: &str,
        onboarding: InfraiError,
    ) -> OnboardingError {
        match self.client.delete_domain(domain, zone_id).await {
            Ok(_) => onboarding.into(),
            Err(cleanup) => OnboardingError::CleanupFailed {
                onboarding: onboarding.to_string(),
                cleanup: cleanup.to_string(),
            },
        }
    }

    pub async fn receive_verification(
        &self,
        signature_hex: &str,
        body: &[u8],
    ) -> Result<Option<DomainState>, OnboardingError> {
        let signature =
            hex::decode(signature_hex).map_err(|_| OnboardingError::InvalidSignature)?;
        let mut mac = HmacSha256::new_from_slice(self.secret.as_bytes())
            .map_err(|_| OnboardingError::InvalidSignature)?;
        mac.update(body);
        mac.verify_slice(&signature)
            .map_err(|_| OnboardingError::InvalidSignature)?;

        let event: VerificationEvent = serde_json::from_slice(body)?;
        let mut states = self.states.write().await;
        let Some(state) = states.get_mut(&event.domain) else {
            return Ok(None);
        };
        if event.verified {
            *state = DomainState::Ready;
        }
        Ok(Some(state.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn signed_result_readies_only_the_registered_game_domain() {
        let service = TenantOnboarding::new(
            InfraiClient::at(String::new(), "http://127.0.0.1".to_owned()),
            "test-signing-secret".to_owned(),
        );
        service
            .states
            .write()
            .await
            .insert("play.arcade.test".to_owned(), DomainState::AwaitingDns);
        let body = br#"{"domain":"play.arcade.test","verified":true}"#;
        let mut mac = HmacSha256::new_from_slice(b"test-signing-secret").unwrap();
        mac.update(body);
        let signature = hex::encode(mac.finalize().into_bytes());

        let result = service
            .receive_verification(&signature, body)
            .await
            .unwrap();

        assert_eq!(result, Some(DomainState::Ready));
        let unknown = br#"{"domain":"other.arcade.test","verified":true}"#;
        let mut unknown_mac = HmacSha256::new_from_slice(b"test-signing-secret").unwrap();
        unknown_mac.update(unknown);
        let unknown_signature = hex::encode(unknown_mac.finalize().into_bytes());
        assert_eq!(
            service
                .receive_verification(&unknown_signature, unknown)
                .await
                .unwrap(),
            None
        );
    }
}
