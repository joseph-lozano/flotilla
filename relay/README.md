# Flotilla's push relay

A Cloudflare Worker that lets anyone's Flotilla server notify the Flotilla iPhone app. Only the app's team can sign pushes to it, so a server without that key sends through the relay instead.

- **No sign-up.** A phone registers its APNs token here when it pairs with a server. It gets back a random **handle** and a **secret**, makes its own **key**, and gives all three to its server (`push.register`).
- **End-to-end encrypted.** The server encrypts each notification with the phone's key and sends the relay only the ciphertext. The relay asks APNs for a neutral alert ("Flotilla: New activity") carrying it. The app's Notification Service Extension decrypts it and shows the real words; if it can't, the neutral alert shows.
- **A server with its own key** (`flotilla setup --apns-key …`) sends to Apple directly and never uses the relay. `flotilla setup --no-push-relay` turns the relay off.

The code runs on Cloudflare's network, so its outbound `fetch` reaches APNs over HTTP/2. Cloudflare's proxy upgrades the request ([workerd#4841](https://github.com/cloudflare/workerd/issues/4841)). `wrangler dev` can't: locally the relay talks to a fake APNs over HTTP/1.1. The [self-test](#the-self-test) proves the real path on the first deploy.

## What it keeps

One row per handle, in D1:

| Field | What |
| --- | --- |
| `handle` | Random, 16 bytes (base64url) |
| `secret_hash` | SHA-256 of the secret; the secret itself is never kept |
| `token`, `topic`, `sandbox` | The phone's APNs token, the app's bundle id, which APNs |
| `created_at`, `used_at` | Unix seconds |

Plus the APNs provider token it signs with (a JWT), shared by the Worker's instances because Apple refuses one made more often than every 20 minutes.

Nothing else. It logs nothing (`observability` is off) and never sees what a notification says. The rate limits count addresses in memory only. D1 keeps 30 days of restorable history on Workers Paid (Time Travel, always on), so a deleted row can be restored for that long.

**It forgets a handle when:**
- the phone deletes it (Forget, or pairing with another server);
- APNs says the phone's token is gone (the app was deleted): 410, `BadDeviceToken` or `DeviceTokenNotForTopic`;
- it's been unused for 90 days (a daily cron).

**A phone has at most 10 handles.** One more is refused (409) rather than made by dropping another, because registering needs no proof of the phone: anyone who knew its APNs token could otherwise push its live handles out. Only handles already past their 90 days make room. Someone who knows a phone's token can still fill its 10 and block new registrations, though never the ones it has. App Attest at registration would close that.

## The API

Every request and answer is JSON. Identifiers and secrets travel only in bodies, never in URLs. An error is `{"error": "<why>"}`. Limits are per Cloudflare location and approximate; a refusal is 429 with `retry-after: 60`.

"Per address" means an IPv4 address, or an IPv6 address's /64 (one network can hand out as many addresses in it as it likes). Phones behind one carrier NAT share an IPv4 address, and so do servers behind one: a phone's refused refresh is tried again at its next connection, and a server tries a refused push again after `retry-after`.

### `POST /v1/register`: a phone registers

```json
{"token": "<APNs device token, hex>", "topic": "com.useflotilla.flotilla", "env": "sandbox" | "production"}
```

| Answer | When |
| --- | --- |
| `201 {"handle": "<22 chars>", "secret": "<43 chars>"}` | Registered |
| `400` | Not a token (hex, 16 to 400 digits) or an environment |
| `403` | A topic this relay doesn't serve (`TOPICS`) |
| `409` | The phone has 10 handles already: `{"error": "…", "code": "too_many"}` |
| `429` | 20 a minute per address, with `refresh` and `unregister` |

### `POST /v1/refresh`: the phone's token changed, or it's still here

```json
{"handle": "…", "secret": "…", "token": "<hex>", "env": "sandbox" | "production"}
```

`204`, or `404` for an unknown handle or a wrong secret (alike, `{"error": "No such handle", "code": "no_such_handle"}`), `409` if the new token has 10 handles already (the handle stays on its old one), `400`, `429`. It counts as use.

### `POST /v1/unregister`: the phone leaves

```json
{"handle": "…", "secret": "…"}
```

`204`, or `404` (unknown, wrong secret, or already gone), `429`.

### `POST /v1/push`: a server sends a notification

```json
{"handle": "…", "secret": "…", "ciphertext": "<base64>", "collapse": "<optional: up to 64 of A-Z a-z 0-9 - _>"}
```

| Answer | When |
| --- | --- |
| `204` | APNs took it |
| `400` | Not base64, or a bad collapse id |
| `404` | Unknown handle or wrong secret, `{"error": "No such handle", "code": "no_such_handle"}`: stop using it. A 404 without that code (a wrong path, a host the relay isn't on) isn't this |
| `410` | APNs says the phone's gone; the handle is dropped. Stop using it |
| `413` | Ciphertext over 3,000 characters |
| `429` | 30 a minute per handle, 300 per address; or APNs' own `TooManyRequests` |
| `502` | APNs refused it (the reason), or didn't answer |
| `503` | The relay has no APNs key yet |

What it asks APNs for, with `apns-push-type: alert`, priority 10, a 24-hour expiry and the collapse id:

```json
{"aps": {"alert": {"title": "Flotilla", "body": "New activity"}, "mutable-content": 1, "sound": "default"}, "e": "<ciphertext>"}
```

### The encryption (between the server and the phone)

The relay never decrypts anything. For anyone writing a server or an app:
- ChaCha20-Poly1305, with the phone's 32-byte key (base64 in `push.register`), a random 12-byte nonce and no associated data.
- `ciphertext` is base64 (standard, padded) of nonce ‖ ciphertext ‖ tag, which is CryptoKit's `ChaChaPoly.SealedBox.combined`.
- The plaintext is the JSON the server would have sent APNs itself: `aps.alert.title` and `body`, `aps.thread-id`, and `flotilla` (`server`, `kind`, `thread`) for where a tap goes.
- `collapse` is the first 32 hex digits of HMAC-SHA256(key, `"collapse:" + <kind>:<id>`), so a newer notification about the same thing replaces the last without the relay learning what it's about.
- `test/vectors.json` has vectors, made with Node (`script/make-vectors.mjs`) and checked by the Rust and Swift tests.

### `GET /v1/selftest`: is APNs reachable?

Only with `authorization: Bearer <SELFTEST_TOKEN>`, and only when that secret is set (else `404`). It sends APNs one request for a device token of zeros. Without a key (or with `?dummy`), the request is signed by no one. `?env=production` goes to production APNs; the default is the sandbox.

```json
{"ok": true, "status": 403, "reason": "InvalidProviderToken", "apns_id": true, "key": "none", "says": "APNs answered: 403 InvalidProviderToken"}
```

- `ok` is true only if APNs' own JSON came back, which needs HTTP/2.
- `key` is `accepted` once the relay's key gets it past the provider token (APNs then says `BadDeviceToken`), and `refused` if APNs refuses the key.

`GET /` answers `Flotilla push relay`.

## Running it here

```sh
npm ci
npm test          # the endpoints, in the Workers runtime, against test/fake-apns.mjs
npm run check     # TypeScript
script/local      # the Worker under wrangler dev on :8796, with the fake APNs on :8797
script/e2e        # that, plus the server's sender against it (push.rs: relay_end_to_end)
```

A server uses a local relay with `FLOTILLA_PUSH_RELAY=http://127.0.0.1:8796`. A debug build of the app uses it when launched with `-FlotillaPushRelay http://127.0.0.1:8796`.

## Deploying it

You need a Cloudflare account that has the `useflotilla.com` zone, on **Workers Paid**, and Node. Everything below runs in `relay/`.

**0. Workers Paid, a rate-limiting rule, and usage alerts.**

- **Workers Paid is required.** The relay's own limits run inside the Worker, so every request, refused or not, is a Worker request; without Paid, a daily request cap one client could use up would switch the relay off for everyone until midnight UTC. In the dashboard: Workers & Pages → Plans → Workers Paid.
- **What it costs** ([Cloudflare's pricing](https://developers.cloudflare.com/workers/platform/pricing/), as of November 2026):
  - $5 a month for the account, with 10 million requests and 30 million CPU milliseconds a month included;
  - then $0.30 per million requests and $0.02 per million CPU milliseconds;
  - D1 on Workers Paid includes 25 billion rows read, 50 million rows written and 5 GB of storage a month;
  - the rate-limit binding has no charge of its own.

  A notification is one request, a few rows read, and at most one row written per phone an hour.
- **Recommended: a rate-limiting rule on the zone,** to cap what a flood can cost. Cloudflare's [Workers metrics page](https://developers.cloudflare.com/workers/observability/metrics-and-analytics/) says, of a Worker's total requests: "Requests blocked by WAF or other security features will not count." In the dashboard, open the `useflotilla.com` zone, then Security → Security rules → Create rule → Rate limiting rules:
  - **Rule name:** Push relay
  - **If incoming requests match:**
    - **On a Free zone,** the only field offered is the path: Field *URI Path*, Operator *starts with* (or *contains*), Value `/v1/`. Only the relay serves `/v1/` on this zone; the site doesn't use it.
    - **On Pro and up:** Field *Hostname*, Operator *equals*, Value `push.useflotilla.com`.
  - **With the same characteristics:** IP
  - **When rate exceeds:** 100 requests per 10 seconds (a Free zone allows only 10-second periods and one rule; on Pro, 300 per 1 minute)
  - **Then take action:** Block, **Duration** 10 seconds (on Pro, up to an hour)
  - **Deploy.**

  That's well above what the relay's own limits let one address do (300 pushes and 20 registrations a minute), so it only stops floods.
- **Usage alerts:** Notifications → Add → Usage Based Billing (billable usage), for Workers requests, with a threshold below the included 10 million (say 5 million a month), sent to your email. It only informs; it doesn't cap anything.

**1. Deploy, and prove the Worker reaches APNs (no Apple key yet).**

```sh
npm ci
npx wrangler login
npx wrangler d1 create flotilla-push-relay
#   paste the database_id it prints into wrangler.toml
npx wrangler d1 migrations apply DB --remote
TOKEN=$(openssl rand -hex 32); echo "$TOKEN"     # keep it for the checks below
echo "$TOKEN" | npx wrangler secret put SELFTEST_TOKEN
npx wrangler deploy                               # push.useflotilla.com, DNS included
curl -s -H "authorization: Bearer $TOKEN" https://push.useflotilla.com/v1/selftest
```

**Pass:** `"ok":true,"status":403,"reason":"InvalidProviderToken"`. That's APNs' own answer, so the Worker reached it over HTTP/2.

**Fail** (`"ok":false`, a Cloudflare or HTML error): the Worker can't reach APNs. Stop, and run the same API from a small server instead (see [Another relay](#another-relay)); the app and the server don't change.

**2. Give it the APNs key.** Flotilla's key already exists: `AuthKey_N3ZKNGG386.p8`, team `64D756D77S`, both already in `wrangler.toml`. (A new one: developer.apple.com → Certificates, Identifiers & Profiles → Keys → +, Apple Push Notifications service; it downloads once.)

```sh
npx wrangler secret put APNS_KEY < AuthKey_N3ZKNGG386.p8
npx wrangler deploy
curl -s -H "authorization: Bearer $TOKEN" https://push.useflotilla.com/v1/selftest
curl -s -H "authorization: Bearer $TOKEN" "https://push.useflotilla.com/v1/selftest?env=production"
```

**Pass:** `"key":"accepted"` from both (APNs got past the key and refused only the made-up device token). Step 1's check with `?dummy` still passes too.

**3.** Install a TestFlight build, pair it with a server that has no key, and check a notification arrives with its real words (the checklist in `ios/README.md`).

## Another relay

The relay is open source with the rest of Flotilla. If you build your own iPhone app (your own bundle id and team), deploy your own copy:
- set `TOPICS` to your bundle id, `APNS_KEY_ID` and `APNS_TEAM_ID` to your key's, and the `routes` to your domain;
- build the app with `FLOTILLA_PUSH_RELAY` (its build setting) set to your relay;
- on servers: `flotilla setup --push-relay https://push.example.com`.

Anything that implements the API above works in its place: the server and the app only speak these four requests.
