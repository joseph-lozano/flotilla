// APNs: the provider token (an ES256 JWT made from the team's key) and one notification.
// The same rules as the server's own sender (crates/flotilla-core/src/push.rs).

export const SANDBOX = "https://api.sandbox.push.apple.com";
export const PRODUCTION = "https://api.push.apple.com";

/// A provider token is made again after this. APNs takes one up to an hour old, and refuses
/// new ones more often than every 20 minutes.
const TOKEN_EVERY_SECS = 40 * 60;

export interface ApnsEnv {
  DB: D1Database;
  APNS_KEY?: string;
  APNS_KEY_ID?: string;
  APNS_TEAM_ID?: string;
  /// Tests: where APNs is.
  APNS_ORIGIN_SANDBOX?: string;
  APNS_ORIGIN_PRODUCTION?: string;
}

export function origin(env: ApnsEnv, sandbox: boolean): string {
  return sandbox ? env.APNS_ORIGIN_SANDBOX || SANDBOX : env.APNS_ORIGIN_PRODUCTION || PRODUCTION;
}

/// Whether the relay has a key to sign with.
export function configured(env: ApnsEnv): boolean {
  return Boolean(env.APNS_KEY?.trim() && appleId(env.APNS_KEY_ID) && appleId(env.APNS_TEAM_ID));
}

/// Apple's ids are 10 upper-case letters and digits.
function appleId(s: string | undefined): s is string {
  return typeof s === "string" && /^[A-Z0-9]{10}$/.test(s.trim());
}

export function b64url(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

const text = new TextEncoder();

/// The key's DER, from the .p8 file's PEM (BEGIN PRIVATE KEY).
function der(pem: string): Uint8Array {
  const body = pem
    .split("\n")
    .filter((l) => !l.startsWith("-----"))
    .join("")
    .replace(/\s/g, "");
  return Uint8Array.from(atob(body), (c) => c.charCodeAt(0));
}

let imported: { pem: string; key: CryptoKey } | undefined;

/// `APNS_KEY` isn't a P-256 private key in PKCS#8 (a .p8 file's contents).
export class UnusableKey extends Error {}

async function signingKey(pem: string): Promise<CryptoKey> {
  if (imported?.pem !== pem) {
    let key: CryptoKey;
    try {
      key = await crypto.subtle.importKey("pkcs8", der(pem), { name: "ECDSA", namedCurve: "P-256" }, false, ["sign"]);
    } catch {
      throw new UnusableKey("The relay's APNs key isn't usable");
    }
    imported = { pem, key };
  }
  return imported.key;
}

/// What the JWT signs: its header and claims, as APNs documents them.
export function signingInput(keyId: string, teamId: string, issuedSecs: number): string {
  const header = `{"alg":"ES256","kid":"${keyId}"}`;
  const claims = `{"iss":"${teamId}","iat":${issuedSecs}}`;
  return `${b64url(text.encode(header))}.${b64url(text.encode(claims))}`;
}

/// The provider token issued at `issuedSecs`. WebCrypto's ECDSA signature is r‖s already,
/// as a JWT wants it.
export async function jwt(env: ApnsEnv, issuedSecs: number): Promise<string> {
  const input = signingInput(env.APNS_KEY_ID!.trim(), env.APNS_TEAM_ID!.trim(), issuedSecs);
  const key = await signingKey(env.APNS_KEY!.trim());
  const sig = await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, key, text.encode(input));
  return `${input}.${b64url(new Uint8Array(sig))}`;
}

let cached: { token: string; issued: number } | undefined;
/// The token this instance is making or reading now: callers meanwhile share it.
let pending: Promise<string> | undefined;

interface Shared {
  token: string;
  issued_at: number;
}

const shared = (env: ApnsEnv) =>
  env.DB.prepare("SELECT token, issued_at FROM provider_token WHERE id = 1").first<Shared>();

/// Offer `token` as the one every instance uses, in place of `seen` (what was there when it was
/// made, or none): it's kept only if no other instance replaced `seen` first. The token kept,
/// whichever instance made it.
export async function publish(env: ApnsEnv, token: string, issued: number, seen: string | null): Promise<Shared> {
  await env.DB.prepare(
    "INSERT INTO provider_token (id, token, issued_at) VALUES (1, ?1, ?2) ON CONFLICT (id) DO UPDATE SET token = excluded.token, issued_at = excluded.issued_at WHERE provider_token.token IS ?3",
  )
    .bind(token, issued, seen)
    .run();
  return (await shared(env)) ?? { token, issued_at: issued };
}

/// The provider token to use now: this instance's, else the one another instance made (kept
/// in D1), else a new one, kept for the others. `rejected`: APNs refused that one, so it's
/// replaced, unless another instance already has.
export async function providerToken(env: ApnsEnv, nowSecs: number, rejected?: string): Promise<string> {
  const young = (issued: number) => nowSecs - issued < TOKEN_EVERY_SECS && issued <= nowSecs + 60;
  if (!rejected && cached && young(cached.issued)) return cached.token;
  const waiting = pending;
  if (waiting) {
    const t = await waiting.catch(() => undefined);
    if (t && t !== rejected) return t;
  }
  const making = (async () => {
    const row = await shared(env);
    if (row && young(row.issued_at) && row.token !== rejected) {
      cached = { token: row.token, issued: row.issued_at };
      return row.token;
    }
    const kept = await publish(env, await jwt(env, nowSecs), nowSecs, row?.token ?? null);
    cached = { token: kept.token, issued: kept.issued_at };
    return kept.token;
  })();
  pending = making;
  try {
    return await making;
  } finally {
    if (pending === making) pending = undefined;
  }
}

/// Tests: forget the token this instance holds.
export function forgetCachedToken() {
  cached = undefined;
  imported = undefined;
  pending = undefined;
}

export interface Notification {
  token: string;
  topic: string;
  sandbox: boolean;
  /// The notification, encrypted for the phone (base64).
  ciphertext: string;
  /// Replaces an earlier one with the same id.
  collapse?: string;
}

/// The only thing the relay ever asks APNs to show: the phone's extension puts in the words.
export function body(ciphertext: string): string {
  return JSON.stringify({
    aps: { alert: { title: "Flotilla", body: "New activity" }, "mutable-content": 1, sound: "default" },
    e: ciphertext,
  });
}

/// What APNs said about one notification.
export type Said =
  | { kind: "sent" }
  /// The token isn't this app's on that phone any more.
  | { kind: "gone" }
  /// The provider token was refused: make one again.
  | { kind: "stale" }
  /// Tokens changed too often (TooManyProviderTokenUpdates): use the one every instance shares.
  | { kind: "churn" }
  | { kind: "busy" }
  | { kind: "refused"; status: number; reason: string };

export function said(status: number, reason: string): Said {
  if (status === 200) return { kind: "sent" };
  if (status === 410 || (status === 400 && (reason === "BadDeviceToken" || reason === "DeviceTokenNotForTopic")))
    return { kind: "gone" };
  if (status === 403 && (reason === "ExpiredProviderToken" || reason === "InvalidProviderToken")) return { kind: "stale" };
  if (status === 429 && reason === "TooManyProviderTokenUpdates") return { kind: "churn" };
  if (status === 429) return { kind: "busy" };
  return { kind: "refused", status, reason };
}

async function post(env: ApnsEnv, n: Notification, providerJwt: string, nowSecs: number): Promise<Said> {
  const headers: Record<string, string> = {
    authorization: `bearer ${providerJwt}`,
    "apns-topic": n.topic,
    "apns-push-type": "alert",
    "apns-priority": "10",
    "apns-expiration": String(nowSecs + 24 * 3600),
    "content-type": "application/json",
  };
  if (n.collapse) headers["apns-collapse-id"] = n.collapse;
  let response: Response;
  try {
    response = await fetch(`${origin(env, n.sandbox)}/3/device/${n.token}`, {
      method: "POST",
      headers,
      body: body(n.ciphertext),
    });
  } catch {
    return { kind: "refused", status: 0, reason: "APNs didn't answer" };
  }
  let reason = "";
  if (response.status !== 200) {
    try {
      const j = (await response.json()) as { reason?: unknown };
      if (typeof j.reason === "string") reason = j.reason;
    } catch {
      // Not APNs' JSON.
    }
  } else {
    await response.body?.cancel();
  }
  return said(response.status, reason);
}

/// Send `n`. Once again if APNs refused the provider token (with a new one, unless another
/// instance made it already), or said tokens change too often (with the shared one).
export async function send(env: ApnsEnv, n: Notification, nowSecs: number): Promise<Exclude<Said, { kind: "churn" }>> {
  const used = await providerToken(env, nowSecs);
  let result = await post(env, n, used, nowSecs);
  if (result.kind === "stale") result = await post(env, n, await providerToken(env, nowSecs, used), nowSecs);
  else if (result.kind === "churn") {
    cached = undefined;
    result = await post(env, n, await providerToken(env, nowSecs), nowSecs);
  }
  return result.kind === "churn" ? { kind: "busy" } : result;
}

/// A JWT-shaped token no key signed: APNs answers it with its JSON reason, which only comes
/// back if the request reached APNs over HTTP/2.
export const DUMMY_JWT = `${signingInput("ABC123DEFG", "DEF123GHIJ", 1700000000)}.${"A".repeat(86)}`;

/// The self-test: one request to APNs for a device token that isn't one. Without a key (or
/// with `dummy`) it's signed by no one, and APNs should say InvalidProviderToken; with the
/// relay's key it should get past the provider token and say BadDeviceToken.
export async function selftest(
  env: ApnsEnv,
  topic: string,
  sandbox: boolean,
  dummy: boolean,
  nowSecs: number,
): Promise<{
  ok: boolean;
  status: number;
  reason: string | null;
  apns_id: boolean;
  key: "none" | "accepted" | "refused" | "unknown";
  says: string;
}> {
  const real = !dummy && configured(env);
  let response: Response;
  try {
    response = await fetch(`${origin(env, sandbox)}/3/device/${"0".repeat(64)}`, {
      method: "POST",
      headers: {
        authorization: `bearer ${real ? await providerToken(env, nowSecs) : DUMMY_JWT}`,
        "apns-topic": topic,
        "apns-push-type": "alert",
        "apns-priority": "10",
        "content-type": "application/json",
      },
      body: JSON.stringify({ aps: { alert: { title: "Flotilla", body: "Self-test" } } }),
    });
  } catch (e) {
    return { ok: false, status: 0, reason: null, apns_id: false, key: "unknown", says: `APNs didn't answer: ${e}` };
  }
  const apnsId = response.headers.has("apns-id");
  const raw = await response.text();
  let reason: string | null = null;
  try {
    const j = JSON.parse(raw) as { reason?: unknown };
    if (typeof j.reason === "string") reason = j.reason;
  } catch {
    // Not APNs' JSON: something between answered (an HTTP/1.1 refusal, a proxy's page).
  }
  const ok = reason !== null;
  const key = !real
    ? "none"
    : reason === "BadDeviceToken"
      ? "accepted"
      : reason === "InvalidProviderToken" || reason === "ExpiredProviderToken" || reason === "MissingProviderToken"
        ? "refused"
        : "unknown";
  const says = ok
    ? `APNs answered: ${response.status} ${reason}`
    : `Not APNs' answer: ${response.status} ${raw.slice(0, 200).replace(/\s+/g, " ")}`;
  return { ok, status: response.status, reason, apns_id: apnsId, key, says };
}
