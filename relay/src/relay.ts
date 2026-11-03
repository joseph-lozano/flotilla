// Flotilla's push relay: a Cloudflare Worker that lets anyone's Flotilla server notify the
// Flotilla iPhone app, whose pushes only the app's team can sign.
//
// - A phone registers its APNs token here and gets a random handle and a secret, which it
//   gives its own server with a key the relay never sees.
// - The server sends the relay a notification encrypted with that key. The relay checks the
//   secret and asks APNs to show a neutral alert carrying the ciphertext; the phone's
//   Notification Service Extension decrypts it and puts in the words.
//
// The API is in README.md. Nothing here logs a token, a secret, a payload or an address.
// The Worker's entry is src/index.ts (a Worker's main module exports only its handler).

import * as apns from "./apns";

export interface Env extends apns.ApnsEnv {
  /// The app bundle ids it serves, comma-separated.
  TOPICS: string;
  /// Turns on GET /v1/selftest for whoever has it.
  SELFTEST_TOKEN?: string;
  /// Per address: registering, refreshing and deleting handles.
  REGISTER_LIMIT: RateLimit;
  /// Per address and per handle: pushes.
  PUSH_IP_LIMIT: RateLimit;
  PUSH_HANDLE_LIMIT: RateLimit;
}

export interface RateLimit {
  limit(options: { key: string }): Promise<{ success: boolean }>;
}

/// A handle unused this long is dropped (the daily sweep), and makes room for another.
export const UNUSED_SECS = 90 * 24 * 3600;
/// A phone (one APNs token) has at most this many handles. One more is refused rather than made
/// by dropping another: registering needs no proof of the phone, so anyone who knew its token
/// could otherwise push its live handles out.
export const HANDLES_PER_TOKEN = 10;
/// The largest ciphertext taken (base64): with the neutral alert it stays under APNs' 4 KB.
export const MAX_CIPHERTEXT = 3000;
const MAX_BODY = 8 * 1024;
/// `used_at` is written at most this often per handle.
const TOUCH_SECS = 3600;

const HANDLE = /^[A-Za-z0-9_-]{22}$/;
const SECRET = /^[A-Za-z0-9_-]{43}$/;
const TOKEN = /^(?:[0-9a-f]{2}){8,200}$/;
const CIPHERTEXT = /^[A-Za-z0-9+/]+={0,2}$/;
const COLLAPSE = /^[A-Za-z0-9_-]{1,64}$/;

function json(status: number, value: unknown, headers: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json", "cache-control": "no-store", ...headers },
  });
}

const error = (status: number, message: string, headers: Record<string, string> = {}) =>
  json(status, { error: message }, headers);
const none = () => new Response(null, { status: 204, headers: { "cache-control": "no-store" } });
const limited = () => error(429, "Too many requests: try again in a minute", { "retry-after": "60" });
const full = () =>
  json(409, {
    error: `This phone has ${HANDLES_PER_TOKEN} registrations with the relay already: forget a server on it, then try again`,
    code: "too_many",
  });

class Refusal extends Error {
  constructor(readonly response: Response) {
    super("refused");
  }
}

/// The request's JSON object, or a refusal. Read a chunk at a time, so a body without a
/// length is refused as soon as it's too large.
async function body(request: Request): Promise<Record<string, unknown>> {
  // JSON only: a form a browser posts without asking first is refused unread.
  const type = (request.headers.get("content-type") ?? "").split(";")[0].trim().toLowerCase();
  if (type !== "application/json") throw new Refusal(error(415, "Send JSON (content-type: application/json)"));
  const tooLarge = () => new Refusal(error(413, "Too large"));
  if (Number(request.headers.get("content-length") ?? "0") > MAX_BODY) throw tooLarge();
  const bytes = new Uint8Array(MAX_BODY);
  let read = 0;
  const reader = request.body?.getReader();
  for (;;) {
    const chunk = await reader?.read();
    if (!chunk || chunk.done) break;
    if (read + chunk.value.byteLength > MAX_BODY) {
      await reader?.cancel();
      throw tooLarge();
    }
    bytes.set(chunk.value, read);
    read += chunk.value.byteLength;
  }
  const text = new TextDecoder().decode(bytes.subarray(0, read));
  try {
    const value = JSON.parse(text);
    if (value && typeof value === "object" && !Array.isArray(value)) return value;
  } catch {
    // Below.
  }
  throw new Refusal(error(400, "Send a JSON object"));
}

function field(o: Record<string, unknown>, name: string, shape: RegExp, what: string): string {
  const v = o[name];
  if (typeof v !== "string" || !shape.test(v)) throw new Refusal(error(400, `${name}: ${what}`));
  return v;
}

function deviceToken(o: Record<string, unknown>): string {
  const v = o.token;
  const token = typeof v === "string" ? v.trim().toLowerCase() : "";
  if (!TOKEN.test(token)) throw new Refusal(error(400, "token: an APNs device token, in hex"));
  return token;
}

function sandbox(o: Record<string, unknown>): boolean {
  if (o.env === "sandbox") return true;
  if (o.env === "production") return false;
  throw new Refusal(error(400, "env: sandbox or production"));
}

const random = (n: number) => apns.b64url(crypto.getRandomValues(new Uint8Array(n)));

async function hash(secret: string): Promise<string> {
  const d = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(secret));
  return [...new Uint8Array(d)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

function same(a: string, b: string): boolean {
  const x = new TextEncoder().encode(a);
  const y = new TextEncoder().encode(b);
  return x.byteLength === y.byteLength && crypto.subtle.timingSafeEqual(x, y);
}

interface Row {
  handle: string;
  secret_hash: string;
  token: string;
  topic: string;
  sandbox: number;
  used_at: number;
}

/// The handle's row, if `secret` is its secret. An unknown handle and a wrong secret look the
/// same to the caller.
async function owned(env: Env, o: Record<string, unknown>): Promise<Row> {
  const handle = field(o, "handle", HANDLE, "the handle the relay gave");
  const secret = field(o, "secret", SECRET, "the secret the relay gave");
  const row = await env.DB.prepare(
    "SELECT handle, secret_hash, token, topic, sandbox, used_at FROM handles WHERE handle = ?1",
  )
    .bind(handle)
    .first<Row>();
  // Marked, so a server tells it from any other 404 (a wrong path, a host Cloudflare doesn't route).
  if (!row || !same(row.secret_hash, await hash(secret)))
    throw new Refusal(json(404, { error: "No such handle", code: "no_such_handle" }));
  return row;
}

const now = () => Math.floor(Date.now() / 1000);
const address = (request: Request) => addressKey(request.headers.get("cf-connecting-ip") ?? "unknown");

/// What the per-address limits count by: an IPv4 address, or an IPv6 address's /64 (one
/// network hands out as many addresses in it as it likes). Phones behind one carrier NAT share
/// an IPv4 key; a refused refresh is retried at their next connection.
export function addressKey(ip: string): string {
  if (!ip.includes(":")) return ip;
  const mapped = /^::ffff:(\d+\.\d+\.\d+\.\d+)$/i.exec(ip);
  if (mapped) return mapped[1];
  const [head, tail] = ip.toLowerCase().split("::", 2);
  const left = head ? head.split(":") : [];
  const right = tail ? tail.split(":") : [];
  const groups = tail === undefined ? left : [...left, ...Array(Math.max(0, 8 - left.length - right.length)).fill("0"), ...right];
  return `${groups
    .slice(0, 4)
    .map((g) => (parseInt(g, 16) || 0).toString(16))
    .join(":")}::/64`;
}

/// A phone's handles past their 90 days, which the sweep would drop anyway, make room
/// (`keep` aside: the one being refreshed).
const expired = (env: Env, token: string, keep: string, t: number) =>
  env.DB.prepare("DELETE FROM handles WHERE token = ?1 AND handle != ?2 AND used_at < ?3").bind(
    token,
    keep,
    t - UNUSED_SECS,
  );

async function register(request: Request, env: Env): Promise<Response> {
  if (!(await env.REGISTER_LIMIT.limit({ key: address(request) })).success) return limited();
  const o = await body(request);
  const token = deviceToken(o);
  const isSandbox = sandbox(o);
  const topic = typeof o.topic === "string" ? o.topic : "";
  if (!topics(env).includes(topic)) return error(403, "topic: this relay doesn't serve that app");
  const handle = random(16);
  const secret = random(32);
  const t = now();
  const [, made] = await env.DB.batch([
    expired(env, token, handle, t),
    // Only under the cap, counted in the same statement.
    env.DB.prepare(
      "INSERT INTO handles (handle, secret_hash, token, topic, sandbox, created_at, used_at) SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?6 WHERE (SELECT count(*) FROM handles WHERE token = ?3) < ?7",
    ).bind(handle, await hash(secret), token, topic, isSandbox ? 1 : 0, t, HANDLES_PER_TOKEN),
  ]);
  if (!made.meta.changes) return full();
  return json(201, { handle, secret });
}

async function refresh(request: Request, env: Env): Promise<Response> {
  if (!(await env.REGISTER_LIMIT.limit({ key: address(request) })).success) return limited();
  const o = await body(request);
  const row = await owned(env, o);
  const token = deviceToken(o);
  const isSandbox = sandbox(o);
  const t = now();
  const [, moved] = await env.DB.batch([
    expired(env, token, row.handle, t),
    // Onto another phone's token only under its cap.
    env.DB.prepare(
      "UPDATE handles SET token = ?2, sandbox = ?3, used_at = ?4 WHERE handle = ?1 AND (token = ?2 OR (SELECT count(*) FROM handles WHERE token = ?2) < ?5)",
    ).bind(row.handle, token, isSandbox ? 1 : 0, t, HANDLES_PER_TOKEN),
  ]);
  if (!moved.meta.changes) return full();
  return none();
}

async function unregister(request: Request, env: Env): Promise<Response> {
  if (!(await env.REGISTER_LIMIT.limit({ key: address(request) })).success) return limited();
  const row = await owned(env, await body(request));
  await env.DB.prepare("DELETE FROM handles WHERE handle = ?1").bind(row.handle).run();
  return none();
}

async function push(request: Request, env: Env): Promise<Response> {
  if (!(await env.PUSH_IP_LIMIT.limit({ key: address(request) })).success) return limited();
  const o = await body(request);
  const ciphertext = o.ciphertext;
  if (typeof ciphertext !== "string" || ciphertext.length % 4 !== 0 || !CIPHERTEXT.test(ciphertext))
    return error(400, "ciphertext: the encrypted notification, in base64");
  if (ciphertext.length > MAX_CIPHERTEXT) return error(413, `ciphertext: at most ${MAX_CIPHERTEXT} characters`);
  const collapse = o.collapse === undefined || o.collapse === null ? undefined : o.collapse;
  if (collapse !== undefined && (typeof collapse !== "string" || !COLLAPSE.test(collapse)))
    return error(400, "collapse: at most 64 letters, digits, - and _");
  const row = await owned(env, o);
  if (!(await env.PUSH_HANDLE_LIMIT.limit({ key: row.handle })).success) return limited();
  if (!apns.configured(env)) return error(503, "The relay has no APNs key");
  const t = now();
  let said;
  try {
    said = await apns.send(env, { token: row.token, topic: row.topic, sandbox: row.sandbox === 1, ciphertext, collapse }, t);
  } catch (e) {
    if (e instanceof apns.UnusableKey) return error(503, e.message);
    throw e;
  }
  switch (said.kind) {
    case "sent":
      if (t - row.used_at >= TOUCH_SECS)
        await env.DB.prepare("UPDATE handles SET used_at = ?2 WHERE handle = ?1").bind(row.handle, t).run();
      return none();
    case "gone": {
      // Only if it's still the token APNs refused: a refresh meanwhile moved it on.
      const dropped = await env.DB.prepare("DELETE FROM handles WHERE handle = ?1 AND token = ?2 AND sandbox = ?3")
        .bind(row.handle, row.token, row.sandbox)
        .run();
      if (!dropped.meta.changes) return error(502, "APNs refused the phone's last token; it has a new one");
      return error(410, "The phone no longer takes notifications: the handle is gone");
    }
    case "busy":
      return limited();
    case "stale":
      return error(502, "APNs refused the relay's provider token");
    case "refused":
      return error(502, `APNs refused it: ${said.status} ${said.reason}`.trim());
  }
}

async function selftest(request: Request, env: Env, url: URL): Promise<Response> {
  const want = env.SELFTEST_TOKEN?.trim();
  const given = (request.headers.get("authorization") ?? "").replace(/^Bearer /, "");
  if (!want) return error(404, "Not found");
  if (!(await env.REGISTER_LIMIT.limit({ key: address(request) })).success) return limited();
  if (!same(given, want)) return error(403, "Forbidden");
  const result = await apns.selftest(
    env,
    topics(env)[0] ?? "com.useflotilla.flotilla",
    url.searchParams.get("env") !== "production",
    url.searchParams.has("dummy"),
    now(),
  );
  return json(result.ok ? 200 : 502, result);
}

function topics(env: Env): string[] {
  return env.TOPICS.split(",")
    .map((t) => t.trim())
    .filter(Boolean);
}

/// Every route, by method and path. Identifiers travel in bodies, never in URLs.
async function route(request: Request, env: Env): Promise<Response> {
  const url = new URL(request.url);
  const at = `${request.method} ${url.pathname}`;
  switch (at) {
    case "GET /":
      return new Response("Flotilla push relay\n", { headers: { "content-type": "text/plain" } });
    case "POST /v1/register":
      return register(request, env);
    case "POST /v1/refresh":
      return refresh(request, env);
    case "POST /v1/unregister":
      return unregister(request, env);
    case "POST /v1/push":
      return push(request, env);
    case "GET /v1/selftest":
      return selftest(request, env, url);
    default:
      return error(404, "Not found");
  }
}

/// Drop handles unused for 90 days.
export async function sweep(env: Env, nowSecs: number): Promise<number> {
  const r = await env.DB.prepare("DELETE FROM handles WHERE used_at < ?1")
    .bind(nowSecs - UNUSED_SECS)
    .run();
  return r.meta.changes ?? 0;
}

/// The Worker (`src/index.ts`).
export const handler = {
  async fetch(request: Request, env: Env): Promise<Response> {
    try {
      return await route(request, env);
    } catch (e) {
      if (e instanceof Refusal) return e.response;
      return error(500, "The relay failed");
    }
  },

  async scheduled(_controller: ScheduledController, env: Env): Promise<void> {
    await sweep(env, now());
  },
} satisfies ExportedHandler<Env>;
