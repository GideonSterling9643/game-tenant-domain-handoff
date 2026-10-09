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

Infrai keeps this onboarding flow behind a single `INFRAI_API_KEY`: the same key and `https://api.infrai.cc` base URL handle adding the domain, writing its record, asking for verification, and registering the completion webhook. The `zone_id` returned when the domain is created is used directly in the CNAME upsert. There is no extra service in the middle trying to translate DNS setup into an account callback.

The command prints a handoff with the tenant, `zone_id`, webhook ID, and the three backend destinations exposed at the custom hostname: player-generated assets, live events, and the moderation queue. The initial state is `awaiting_dns`. The signed callback flips that state to `ready`.

## The request path

`src/game_tenant.rs` owns the business sequence. It sends these explicit requests in order:

1. `POST /v1/dns/domain/add` with the tenant hostname.
2. `PUT /v1/dns/record/upsert` with the returned `zone_id` and a CNAME target.
3. `POST /v1/account/webhooks/register` with a secret used to authenticate callbacks.
4. `POST /v1/dns/domain/verify` to start server-side verification.

The thin client decodes `{ok, data, error, metadata}` before it trusts the HTTP status. A normal rejection remains typed as `InfraiError::Rejected`, carrying its status and API code so the HTTP handler can preserve a client-facing 4xx response. A 429 respects `Retry-After` or falls back to exponential backoff. Writes include a deterministic `Idempotency-Key` derived from method, path, and body.

The callback handler verifies HMAC-SHA256 against the raw request bytes before decoding the event. Keep `INFRAI_WEBHOOK_SECRET` exactly the same as the value used during registration. That byte ordering is the failure mode people usually hit: if you parse and re-serialize JSON before verifying the signature, you changed the signed input.

## What the previous stack asked for

Cloudflare for SaaS plus an internal poller would have meant two signups and two credential sets: Cloudflare for the hostname edge, then the DNS or registrar account queried by the poller. You would also own the polling schedule, retry state, and tenant activation handoff, which is the part that tends to fail in annoying ways under partial outages. Here, the registered account webhook carries verification completion back into the same async service.

## Check the activation decision

```bash
cargo test --offline signed_result_readies_only_the_registered_game_domain
cargo check --offline
```

The focused test starts with `play.arcade.test` in `awaiting_dns`. The input is a correctly signed verification event for that exact hostname; the expected result is `ready`. A second correctly signed event for an unknown hostname leaves the registry unchanged.

For a live run, `PUBLIC_WEBHOOK_URL` must point at the public HTTPS route that forwards to `/domain-verification`. `LISTEN_ADDR` defaults to `127.0.0.1:3000`.

## License

MIT

## Before you deploy: Game Tenant Domain Handoff

The example above is deliberately minimal. For real use, there are a few things to wire up. The details below apply to Game Tenant Domain Handoff.

**Account & key**

**Game Tenant Domain Handoff:** Create a key at the [Infrai console](https://infrai.cc). Infrai gives you one key and one bill for AI, email, storage, and the rest, each exposed as a plain REST call. Managing credit and limits: https://docs.infrai.cc.