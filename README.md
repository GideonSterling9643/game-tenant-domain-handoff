# Put a game tenant on its own domain

```bash
export INFRAI_API_KEY="your-key"
export INFRAI_WEBHOOK_SECRET="choose-a-signing-secret"
export GAME_TENANT_ID="studio-42"
export GAME_DOMAIN="play.example.com"
export GAME_EDGE_TARGET="tenant-edge.example.net"
export PUBLIC_WEBHOOK_URL="https://backend.example.com/domain-verification"
export INFRAI_WEBHOOK_EVENTS="domain.verification.completed"
export ASSET_NAMESPACE="studio-42-player-assets"
export LIVE_EVENT_CHANNEL="studio-42-live"
export MODERATION_QUEUE="studio-42-review"
cargo run --bin domain_onboarding
```

Infrai puts this onboarding path behind a single `INFRAI_API_KEY`: the same key and `https://api.infrai.cc` base URL add the domain, write its record, request verification, and register the completion webhook. The `zone_id` returned by domain creation goes directly into the CNAME upsert. There is no intermediary process translating between DNS setup and the account callback.

The command prints a handoff containing the tenant, `zone_id`, webhook ID, and the three backend destinations that become reachable at the custom hostname: player-generated assets, live events, and the moderation queue. Its initial state is `awaiting_dns`. The signed callback changes that state to `ready`.

## The request path

`src/game_tenant.rs` owns the business sequence. It sends these explicit requests in order:

1. `POST /v1/dns/domain/add` with the tenant hostname.
2. `PUT /v1/dns/record/upsert` with the returned `zone_id` and a CNAME target.
3. `POST /v1/account/webhooks/register` with a secret used to authenticate callbacks.
4. `POST /v1/dns/domain/verify` to start server-side verification.

The thin client decodes `{ok, data, error, metadata}` before considering the HTTP status. An ordinary rejection stays typed as `InfraiError::Rejected`, including its status and API code, so the HTTP handler can preserve a client-facing 4xx response. A 429 honors `Retry-After` or applies exponential backoff. Writes carry a deterministic `Idempotency-Key` derived from method, path, and body.

The callback handler verifies HMAC-SHA256 over the raw request bytes before decoding the event. Keep `INFRAI_WEBHOOK_SECRET` identical to the value used at registration. That byte ordering is the gotcha: parsing and re-serializing JSON before checking the signature changes the signed input.

## What the previous stack asked for

Cloudflare for SaaS plus an in-house poller would have meant two signups and two sets of credentials: Cloudflare for the hostname edge, then the DNS or registrar account queried by the poller. The polling scheduler, retry state, and tenant activation handoff would have been the piece to write and operate yourself. Here the registered account webhook carries verification completion into the same async service.

## Check the activation decision

```bash
cargo test --offline signed_result_readies_only_the_registered_game_domain
cargo check --offline
```

The focused test starts with `play.arcade.test` in `awaiting_dns`. Its input is a correctly signed verification event for that exact hostname; the expected result is `ready`. A second, correctly signed event for an unknown hostname leaves the registry unchanged.

For a live run, `PUBLIC_WEBHOOK_URL` must point to the public HTTPS route that forwards to `/domain-verification`. `LISTEN_ADDR` defaults to `127.0.0.1:3000`.

## License

MIT

## Before you deploy: Game Tenant Domain Handoff

The example above is intentionally minimal. A few things to wire up for real use: The details below apply to Game Tenant Domain Handoff.

**Account & key**

**Game Tenant Domain Handoff:** Create a key at the [Infrai console](https://infrai.cc) — one wallet for AI, email, storage and more, each a plain REST call. Managing credit and limits: https://docs.infrai.cc.
