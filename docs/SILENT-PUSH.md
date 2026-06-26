# Silent Push (APNs) for reliable background refresh

NightKnight is a *follower*: glucose lands on the server (connector sync, an uploader, or a
direct write) and the phone has to find out about it. iOS gives a follower three ways to
update in the background, in increasing order of reliability:

1. **`BGAppRefreshTask`** — opportunistic, heavily throttled by iOS, can be minutes-to-hours
   late or skipped entirely. Already wired (`BackgroundRefresh` in `NightKnightApp.swift`).
2. **HealthKit background delivery** — wakes the app when *Apple Health* gets new glucose.
   Already wired (`HealthKitManager.startBackgroundDelivery`), but only helps when a vendor
   CGM app writes to Health — useless when the data comes from the NightKnight server.
3. **Silent push (APNs background notification)** — the **server tells the phone** "new data,
   wake up." This is the only mechanism that is both timely *and* works for server-sourced
   data. This document describes how to add it.

The app is already entitled for it — `aps-environment` is in `NightKnight.entitlements` and
`remote-notification` is in `UIBackgroundModes`. What's missing is (a) the device registering
its APNs token with the server, (b) the Worker holding an APNs key and sending a push when a
new reading is ingested, and (c) the app handling the push and refreshing.

> **Reality check.** A silent push is a *request*, not a guarantee. iOS rate-limits background
> notifications and delivers them "at a time that conserves power," coalescing or dropping them
> under Low Power Mode, poor connectivity, or if the app is force-quit. In practice it's far
> better than `BGAppRefreshTask` (seconds-to-a-minute when the device is awake and online) but
> it is **best-effort**. Keep `BGAppRefreshTask` and HealthKit delivery as complements; treat
> push as the primary path, not the only one.

---

## Architecture

```
                         (1) registerForRemoteNotifications
  ┌───────────┐  APNs token ─────────────►  ┌──────────────────────────┐
  │  iOS app  │  POST /api/v4/push/register  │  NightKnight Worker / API │
  │           │ ◄───────────────────────────│  push_tokens(user_id,…)   │
  └─────┬─────┘                              └─────────────┬────────────┘
        │ (3) didReceiveRemoteNotification                 │ (2) new reading ingested
        │     → BackgroundRefresh.refreshNow()             │     (connector sync / upload)
        │     → WidgetCenter.reloadAllTimelines()          ▼
        │                                     sign ES256 JWT (APNs key)
        │ ◄───── silent push ──────  api.push.apple.com/3/device/<token>
        ▼          {"aps":{"content-available":1}}   apns-push-type: background
   refreshed UI + widget                              apns-priority: 5
```

1. On launch the app registers for remote notifications and POSTs its APNs device token to
   the server, scoped to the authenticated user.
2. When the server ingests a new reading for a user (the connector sync loop, an uploader
   write, or a CSV/Nightscout import), it sends a silent push to that user's registered
   device token(s).
3. iOS wakes the app in the background; the app runs `BackgroundRefresh.refreshNow()` (pull
   `/current`, mirror to Health, evaluate alarms, reload widget timelines) and tells iOS it
   got new data.

---

## Apple-side prerequisites (one-time)

You provide these; they can't be created from the repo.

1. **Enable Push Notifications** for the App ID in the Apple Developer portal (and the
   "Push Notifications" capability on the `NightKnight` target in Xcode — `aps-environment`
   is already in the entitlements).
2. **Create an APNs Auth Key** (token-based auth, *not* a certificate): Apple Developer →
   Keys → **+** → check **Apple Push Notifications service (APNs)** → download the `.p8`
   **once** (you can't re-download it). Note its **Key ID** (10 chars).
3. Note your **Team ID** (10 chars, Membership page) and the app's **Bundle ID**
   (`be.cooney.nightknight.NightKnight`).

One APNs key works for both the sandbox and production environments and all your apps.

---

## Worker secrets & config

The `.p8` key is a secret — it never goes in the repo. Set it (and the non-secret IDs) on the
Worker:

```sh
cd service/crates/nightknight-worker
# Paste the full PEM, including the BEGIN/END PRIVATE KEY lines:
npx wrangler secret put APNS_KEY_P8
wrangler secret put APNS_KEY_ID     # e.g. ABC1234DEF
wrangler secret put APNS_TEAM_ID    # e.g. XYZ9876WUV
```

Non-secret vars in `wrangler.toml` (`[vars]`):

```toml
APNS_BUNDLE_ID = "be.cooney.nightknight.NightKnight"
# "sandbox" while the build uses aps-environment=development (Xcode debug / direct install);
# "production" for TestFlight / App Store builds. The device also reports which it minted its
# token under (see iOS §"register"), and the per-token `environment` column wins — this is
# only the default for tokens that don't carry one.
APNS_DEFAULT_ENV = "sandbox"
```

The container server (`nightknight-server`) reads the same values from env vars
(`APNS_KEY_P8`, `APNS_KEY_ID`, `APNS_TEAM_ID`, `APNS_BUNDLE_ID`); it can send pushes too and is
handy for local testing (see *Testing*).

---

## Server implementation

### 1. Store device tokens (`push_tokens`)

A small table, one row per (user, device-token). The APNs token is **not** a secret (it only
identifies a device to APNs) but it must be scoped to its owner so a push for user A can never
go to user B's device — the same per-row isolation every other collection uses.

```sql
CREATE TABLE IF NOT EXISTS push_tokens (
  user_id     TEXT NOT NULL,
  token       TEXT NOT NULL,          -- hex APNs device token
  environment TEXT NOT NULL,          -- 'sandbox' | 'production'
  bundle_id   TEXT NOT NULL,
  updated_at  INTEGER NOT NULL,
  PRIMARY KEY (user_id, token)
);
CREATE INDEX IF NOT EXISTS idx_push_tokens_user ON push_tokens(user_id);
```

Add it to both stores (`nightknight-store-d1` and `nightknight-store-sql`) behind a small
trait method on the storage abstraction, mirroring `*_connector_credentials`
(`upsert_push_token`, `list_push_tokens(user_id)`, `delete_push_token`).

### 2. Registration endpoint

Header-only auth like the rest of v4; scope writes to the calling principal.

```
POST /api/v4/push/register      body: { "token": "<hex>", "environment": "sandbox|production" }
DELETE /api/v4/push/register    body: { "token": "<hex>" }     # on sign-out / token change
```

```rust
// nightknight-api/src/v4.rs — sketch
async fn v4_push_register(&self, req, principal, now_ms) -> Result<ApiResponse, ApiError> {
    principal.require(Permission::api("settings", Action::Update))?; // device-token-scoped
    let body = req.body_json()?;
    let token = body.get("token").and_then(|v| v.as_str())
        .filter(|t| t.len() >= 16 && t.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| ApiError::BadRequest("token must be a hex APNs device token".into()))?;
    let env = match body.get("environment").and_then(|v| v.as_str()) {
        Some("production") => "production",
        _ => "sandbox",
    };
    self.storage.upsert_push_token(&principal.user.id, token, env, APNS_BUNDLE_ID, now_ms).await?;
    Ok(ApiResponse::json(200, &json!({ "ok": true })))
}
```

### 3. The APNs provider token (ES256 JWT)

APNs token-based auth wants a short JWT signed with **ES256** (ECDSA P-256 + SHA-256). It's
valid for **one hour** and reusable across every push in that window — sign it lazily and cache
it (regenerate when it's older than ~50 min; APNs rejects tokens older than 60 min and *also*
rejects regenerating "too often", so don't sign per-push).

```
header  = { "alg": "ES256", "kid": "<APNS_KEY_ID>" }
claims  = { "iss":  "<APNS_TEAM_ID>", "iat": <unix seconds> }
jwt     = base64url(header) + "." + base64url(claims) + "." + base64url(ES256_sign(...))
```

In the Rust wasm Worker, sign with the pure-Rust **`p256`** crate (RustCrypto — compiles to
`wasm32-unknown-unknown`, no `ring`):

```rust
use p256::ecdsa::{signature::Signer, Signature, SigningKey};
use p256::pkcs8::DecodePrivateKey;

fn apns_jwt(p8_pem: &str, key_id: &str, team_id: &str, now_s: i64) -> String {
    let header = b64url(&json!({ "alg": "ES256", "kid": key_id }));
    let claims = b64url(&json!({ "iss": team_id, "iat": now_s }));
    let signing_input = format!("{header}.{claims}");
    let key = SigningKey::from_pkcs8_pem(p8_pem).expect("valid .p8");
    let sig: Signature = key.sign(signing_input.as_bytes()); // r||s, 64 bytes (P1363)
    format!("{signing_input}.{}", base64_url_no_pad(&sig.to_bytes()))
}
```

> Alternative: Cloudflare's Web Crypto (`crypto.subtle.sign({name:"ECDSA", hash:"SHA-256"}, …)`
> produces the same P1363 `r||s` signature and avoids a Rust crypto dep, at the cost of a
> JS-bridge call from wasm. The `p256` crate keeps it all in Rust and is the smaller change.

### 4. Send a push

```rust
// host: sandbox vs production per the token's `environment` column
let host = if env == "production" { "https://api.push.apple.com" }
           else { "https://api.sandbox.push.apple.com" };
let url = format!("{host}/3/device/{token}");
let body = json!({ "aps": { "content-available": 1 } }).to_string(); // SILENT: no alert/sound

let resp = http.send(HttpReq::post(url, vec![
    ("authorization".into(), format!("bearer {jwt}")),
    ("apns-topic".into(),       APNS_BUNDLE_ID.into()),
    ("apns-push-type".into(),   "background".into()),  // REQUIRED for content-available
    ("apns-priority".into(),    "5".into()),           // background MUST be 5 (10 is rejected)
    ("apns-expiration".into(),  "0".into()),            // don't store-and-retry a stale "wake up"
    ("apns-collapse-id".into(), "glucose".into()),      // coalesce: only the newest matters
])).await?;

match resp.status {
    200 => {}                          // delivered to APNs
    410 => self.storage.delete_push_token(user_id, token).await?, // device unregistered → prune
    400 | 403 => log_apns_error(&resp),// BadDeviceToken / bad JWT / wrong env — see body `reason`
    429 => { /* TooManyRequests — back off */ }
    _   => log_apns_error(&resp),
}
```

Key choices:
- **`apns-push-type: background` + `apns-priority: 5` + `content-available: 1`, no alert/sound** —
  this is the definition of a silent push. Priority 10 or a missing `apns-push-type` gets the
  push **rejected**.
- **`apns-expiration: 0`** — a "new data" nudge is worthless if delivered an hour later; don't
  let APNs store-and-forward it.
- **`apns-collapse-id`** — successive readings supersede each other; collapsing avoids a backlog
  of wake-ups.
- **`410 Unregistered`** → delete the token. APNs is the source of truth for token validity;
  prune eagerly or you'll send into the void forever.

### 5. Trigger it

Push **after** a reading is actually stored, so the phone always finds something new. The
natural hook is the ingest path: after `ingest_for_user_id` reports `imported > 0` for a user
(connector sync, uploader write, import), enqueue a push to that user's tokens. Coalesce so a
burst of imports yields **one** push:

- In the Worker's per-minute cron, after `sync_connectors`, push once per user that gained
  readings this tick.
- For direct uploads (`POST /api/v1|v3/entries`), push after the write if `created > 0`,
  debounced to at most one push per user per ~60 s (a `last_pushed_at` column or a short-lived
  KV/DO flag).

Don't push on duplicates or on the bulk Nightscout backfill (a one-time history import isn't
"new" and would fire a useless wake-up).

### HTTP/2 note (important)

APNs **requires HTTP/2**. On a deployed Cloudflare Worker, `fetch()` to `api.push.apple.com`
works (Cloudflare negotiates HTTP/2 at the edge). It does **not** work under `wrangler dev` on
macOS — `fetch()` there is HTTP/1.1 and APNs closes the connection ([workerd#4841](https://github.com/cloudflare/workerd/issues/4841)).
So: **test against the deployed Worker**, or use the `nightknight-server` container (reqwest
speaks HTTP/2) for local end-to-end testing.

---

## iOS implementation

The app is pure SwiftUI with no app delegate. Add one via `UIApplicationDelegateAdaptor` to
catch the registration callbacks and the incoming push.

```swift
// NightKnightApp.swift
@main
struct NightKnightApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    // … existing body …
}

final class AppDelegate: NSObject, UIApplicationDelegate {
    func application(_ app: UIApplication,
                     didFinishLaunchingWithOptions _: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        // Silent pushes (content-available, no alert) need NO user permission prompt —
        // just register for remote notifications. (Only user-visible alerts need authorization.)
        app.registerForRemoteNotifications()
        return true
    }

    func application(_ app: UIApplication,
                     didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        let hex = deviceToken.map { String(format: "%02x", $0) }.joined()
        Task { await PushRegistration.send(apnsToken: hex) } // POST /api/v4/push/register
    }

    func application(_ app: UIApplication,
                     didFailToRegisterForRemoteNotificationsWithError error: Error) {
        // No APNs in the simulator and no entitlement in some dev setups — log, don't crash.
    }

    // The silent push lands here. You have ~30s of background runtime; call the handler.
    func application(_ app: UIApplication,
                     didReceiveRemoteNotification userInfo: [AnyHashable: Any],
                     fetchCompletionHandler completion: @escaping (UIBackgroundFetchResult) -> Void) {
        Task { @MainActor in
            await BackgroundRefresh.refreshNow()     // pull /current, mirror Health, alarms, reload widget
            completion(.newData)
        }
    }
}
```

`PushRegistration.send` posts the token + environment to the server using the existing
`APIClient`/`Settings`:

```swift
enum PushRegistration {
    static func send(apnsToken: String) async {
        let settings = Settings.shared
        guard settings.isConfigured else { return }
        #if DEBUG
        let env = "sandbox"        // development aps-environment → sandbox APNs
        #else
        let env = "production"
        #endif
        try? await APIClient(settings: settings).registerPush(token: apnsToken, environment: env)
    }
}
```

Notes:
- **No permission prompt.** Silent pushes don't require `UNUserNotificationCenter`
  authorization, so this stays consistent with the app's "don't prompt on launch" rule.
  (If you later add *visible* low-glucose alerts pushed from the server, that path *does* need
  authorization — keep them separate.)
- **Re-register on token change.** iOS can rotate the APNs token; `didRegister…` fires again —
  always re-POST. Also call `registerForRemoteNotifications()` on every launch (cheap; returns
  the cached token fast).
- **`aps-environment` must match the host.** A token minted by a `development`-entitled build
  only works against `api.sandbox.push.apple.com`; a TestFlight/App-Store build against
  `api.push.apple.com`. That's why the device sends `environment` and the server keys off it —
  mixing them yields `400 BadDeviceToken`.

---

## Payload & headers reference

| Header | Value | Why |
|---|---|---|
| `:method` / path | `POST /3/device/<hex-token>` | per-device endpoint |
| `authorization` | `bearer <ES256 JWT>` | token-based provider auth |
| `apns-topic` | `be.cooney.nightknight.NightKnight` | the app's bundle id |
| `apns-push-type` | `background` | **required** for `content-available` |
| `apns-priority` | `5` | background pushes must be 5 (10 ⇒ rejected) |
| `apns-expiration` | `0` | drop if not deliverable now |
| `apns-collapse-id` | `glucose` | coalesce successive wake-ups |

Payload (silent — note: **no** `alert`, `sound`, or `badge`):

```json
{ "aps": { "content-available": 1 } }
```

Hosts: `https://api.push.apple.com` (production) · `https://api.sandbox.push.apple.com` (development).

---

## Testing

- **A real device is required** — the iOS Simulator has no APNs and never returns a device
  token. (You *can* unit-test the Worker's JWT signing and the registration endpoint in the
  simulator/CI; the actual push needs a device.)
- End-to-end: install a debug build on a device, confirm `POST /api/v4/push/register` lands a
  row, then ingest a reading server-side and watch the app wake. Inspect with the Worker's
  observability logs (already enabled) — log the APNs status code + `reason`.
- Because `wrangler dev` can't do HTTP/2 to APNs on macOS, drive the *send* from either the
  **deployed Worker** or the **container** (`reqwest` HTTP/2). A quick manual probe from a
  machine with HTTP/2 (`nghttp`/`curl --http2`):
  ```sh
  curl --http2 -v \
    -H "authorization: bearer $JWT" \
    -H "apns-topic: be.cooney.nightknight.NightKnight" \
    -H "apns-push-type: background" -H "apns-priority: 5" \
    -d '{"aps":{"content-available":1}}' \
    https://api.sandbox.push.apple.com/3/device/$DEVICE_TOKEN
  ```
- Common failures: `400 BadDeviceToken` (wrong env/host for the token), `403
  InvalidProviderToken` (bad/expired JWT, wrong Key ID/Team ID), `403 TopicDisallowed`
  (`apns-topic` ≠ bundle id), `410 Unregistered` (prune the token).

---

## Security

- The `.p8` key is a Worker **secret** (`wrangler secret put`) — never commit it. Rotate by
  creating a new key and re-setting the secret; old keys can be revoked in the portal.
- APNs device tokens aren't secrets but are **per-user**: only ever send to tokens owned by the
  reading's user (`list_push_tokens(user_id)`), enforced by the same row-isolation as
  everything else. The registration endpoint writes only to the caller's rows.
- Prune on `410 Unregistered` and on explicit sign-out (`DELETE /api/v4/push/register`).
- A silent push carries **no** glucose data in the payload (just `content-available`), so even
  if a push were misrouted it leaks nothing — the app fetches the actual reading over the
  authenticated, Access-gated API.

---

## Implementation checklist

- [ ] Apple: enable Push Notifications for the App ID; create the APNs `.p8` key; note Key ID,
      Team ID, Bundle ID.
- [ ] Worker: `wrangler secret put APNS_KEY_P8 / APNS_KEY_ID / APNS_TEAM_ID`; add
      `APNS_BUNDLE_ID` + `APNS_DEFAULT_ENV` vars.
- [ ] Storage: `push_tokens` table + `upsert_push_token` / `list_push_tokens` /
      `delete_push_token` in both stores.
- [ ] API: `POST`/`DELETE /api/v4/push/register` (header-auth, per-user).
- [ ] Worker: ES256 JWT signer (`p256`) with ~50-min cache; `send_push(token, env)`; trigger
      after `imported > 0` (coalesced, skip the bulk Nightscout backfill).
- [ ] iOS: `UIApplicationDelegateAdaptor` → register, send token to server, handle the silent
      push by calling `BackgroundRefresh.refreshNow()` + `completion(.newData)`.
- [ ] Verify on a real device against the deployed Worker; log APNs status; prune on 410.
- [ ] Keep `BGAppRefreshTask` + HealthKit delivery as complements; push is best-effort.

---

## References

- [Establishing a token-based connection to APNs](https://developer.apple.com/documentation/usernotifications/establishing-a-token-based-connection-to-apns)
- [Sending notification requests to APNs](https://developer.apple.com/documentation/usernotifications/sending-notification-requests-to-apns)
- [Pushing background updates to your App](https://developer.apple.com/documentation/usernotifications/pushing-background-updates-to-your-app)
- [Cloudflare Workers Web Crypto (`crypto.subtle`)](https://developers.cloudflare.com/workers/runtime-apis/web-crypto/)
- [workerd#4841 — APNs HTTP/2 via `fetch()` works on Workers, not local macOS](https://github.com/cloudflare/workerd/issues/4841)
- [`p256` crate (RustCrypto, wasm-friendly ES256)](https://docs.rs/p256)
