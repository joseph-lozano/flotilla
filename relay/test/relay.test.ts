// The relay's endpoints, against the fake APNs (test/fake-apns.mjs), in the Workers runtime.
import { SELF, createScheduledController, env } from "cloudflare:test";
import { describe, expect, it, beforeEach } from "vitest";
import { HANDLES_PER_TOKEN, MAX_CIPHERTEXT, UNUSED_SECS, addressKey, handler as worker, sweep } from "../src/relay";
import { DUMMY_JWT, forgetCachedToken, jwt, providerToken, publish } from "../src/apns";

const PHONE = "a1".repeat(32);
const TOPIC = "com.useflotilla.flotilla";
/// Base64, as a server sends it (the relay never reads it).
const CIPHERTEXT = btoa("nonce-12byte" + "x".repeat(60) + "tag-16-bytes----");

let ip = 0;
/// Each test its own address, so one's limits don't touch another's.
function address() {
  ip += 1;
  return `198.51.100.${ip}`;
}

function post(path: string, body: unknown, from = "203.0.113.9") {
  return SELF.fetch(`https://push.example.com${path}`, {
    method: "POST",
    headers: { "content-type": "application/json", "cf-connecting-ip": from },
    body: typeof body === "string" ? body : JSON.stringify(body),
  });
}

interface Sent {
  env: string;
  token: string;
  jwt: string;
  jwtValid: boolean;
  topic: string;
  pushType: string;
  priority: string;
  expiration: string;
  collapse: string | null;
  body: string;
}

async function apnsLog(): Promise<Sent[]> {
  return (await fetch(`${env.FAKE_APNS}/_log`)).json();
}

async function apnsAnswers(answer: { status: number; reason?: string; raw?: string; delay?: number }) {
  await fetch(`${env.FAKE_APNS}/_answer`, { method: "POST", body: JSON.stringify(answer) });
}

let phones = 0;
/// A phone's token of its own, so no test fills another's ten handles.
function phone() {
  phones += 1;
  return phones.toString(16).padStart(4, "0") + "a1".repeat(30);
}

async function register(token = phone(), extra: Record<string, unknown> = {}, from = address()) {
  const r = await post("/v1/register", { token, topic: TOPIC, env: "sandbox", ...extra }, from);
  expect(r.status).toBe(201);
  return (await r.json()) as { handle: string; secret: string };
}

async function row(handle: string) {
  return env.DB.prepare("SELECT * FROM handles WHERE handle = ?1").bind(handle).first<Record<string, unknown>>();
}

beforeEach(async () => {
  await apnsLog();
});

describe("register", () => {
  it("gives a random handle and secret, and keeps only the secret's hash", async () => {
    const { handle, secret } = await register(PHONE.toUpperCase());
    expect(handle).toMatch(/^[A-Za-z0-9_-]{22}$/);
    expect(secret).toMatch(/^[A-Za-z0-9_-]{43}$/);
    const kept = await row(handle);
    expect(kept).toMatchObject({ token: PHONE, topic: TOPIC, sandbox: 1 });
    expect(Object.keys(kept!).sort()).toEqual(
      ["created_at", "handle", "sandbox", "secret_hash", "token", "topic", "used_at"].sort(),
    );
    expect(kept!.secret_hash).toMatch(/^[0-9a-f]{64}$/);
    expect(JSON.stringify(kept)).not.toContain(secret);
    const again = await register();
    expect(again.handle).not.toBe(handle);
    expect(again.secret).not.toBe(secret);
  });

  it("takes only JSON", async () => {
    for (const type of ["text/plain", "application/x-www-form-urlencoded", ""]) {
      const r = await SELF.fetch("https://push.example.com/v1/register", {
        method: "POST",
        headers: { "cf-connecting-ip": address(), ...(type ? { "content-type": type } : {}) },
        body: JSON.stringify({ token: PHONE, topic: TOPIC, env: "sandbox" }),
      });
      expect(r.status).toBe(415);
    }
    const charset = await SELF.fetch("https://push.example.com/v1/register", {
      method: "POST",
      headers: { "cf-connecting-ip": address(), "content-type": "application/json; charset=utf-8" },
      body: JSON.stringify({ token: "9a".repeat(32), topic: TOPIC, env: "sandbox" }),
    });
    expect(charset.status).toBe(201);
  });

  it("refuses what isn't a token, an environment or an app it serves", async () => {
    const from = address();
    const bad = async (body: unknown, status: number, says: string) => {
      const r = await post("/v1/register", body, from);
      expect(r.status).toBe(status);
      expect(((await r.json()) as { error: string }).error).toContain(says);
    };
    await bad({ token: "xyz", topic: TOPIC, env: "sandbox" }, 400, "token");
    await bad({ token: "abc", topic: TOPIC, env: "sandbox" }, 400, "token");
    await bad({ token: PHONE, topic: TOPIC, env: "staging" }, 400, "env");
    await bad({ token: PHONE, topic: "com.example.other", env: "sandbox" }, 403, "topic");
    await bad("[1]", 400, "JSON object");
    await bad("not json", 400, "JSON object");
    const big = await post("/v1/register", { token: PHONE, topic: TOPIC, env: "sandbox", pad: "x".repeat(9000) }, from);
    expect(big.status).toBe(413);
  });

  it("refuses a body over its limit as it comes, without a length", async () => {
    const chunk = new TextEncoder().encode(" ".repeat(1024));
    let sent = 0;
    const stream = new ReadableStream<Uint8Array>({
      pull(c) {
        sent += 1;
        if (sent > 64) c.close();
        else c.enqueue(chunk);
      },
    });
    const r = await SELF.fetch("https://push.example.com/v1/register", {
      method: "POST",
      headers: { "cf-connecting-ip": address(), "content-type": "application/json" },
      body: stream,
    });
    expect(r.status).toBe(413);
    expect(sent).toBeLessThan(64);
  });

  it("never drops a phone's live handle for another registration", async () => {
    const token = "c3".repeat(32);
    const victim = await register(token);
    // Someone who knows the token registers it again and again, from addresses of their own.
    for (let i = 1; i < HANDLES_PER_TOKEN; i++) await register(token);
    const refused = await post("/v1/register", { token, topic: TOPIC, env: "sandbox" }, address());
    expect(refused.status).toBe(409);
    expect(await refused.json()).toMatchObject({ code: "too_many" });
    // The phone's own handle still works.
    expect((await post("/v1/push", { ...victim, ciphertext: CIPHERTEXT })).status).toBe(204);
    expect(await row(victim.handle)).not.toBeNull();
    const count = async (t: string) =>
      (await env.DB.prepare("SELECT count(*) AS n FROM handles WHERE token = ?1").bind(t).first<{ n: number }>())!.n;
    expect(await count(token)).toBe(HANDLES_PER_TOKEN);
    // Nor by moving another handle onto it: refused, and that handle stays where it was.
    const other = await register("c4".repeat(32));
    const moved = await post("/v1/refresh", { ...other, token, env: "sandbox" }, address());
    expect(moved.status).toBe(409);
    expect(await row(other.handle)).toMatchObject({ token: "c4".repeat(32) });
    expect(await count(token)).toBe(HANDLES_PER_TOKEN);
    // A handle past its 90 days unused (which the sweep would drop) makes room.
    const old = (await env.DB.prepare("SELECT handle FROM handles WHERE token = ?1 AND handle != ?2 LIMIT 1")
      .bind(token, victim.handle)
      .first<{ handle: string }>())!.handle;
    await env.DB.prepare("UPDATE handles SET used_at = ?2 WHERE handle = ?1")
      .bind(old, Math.floor(Date.now() / 1000) - UNUSED_SECS - 1)
      .run();
    expect((await post("/v1/register", { token, topic: TOPIC, env: "sandbox" }, address())).status).toBe(201);
    expect(await row(old)).toBeNull();
    expect(await row(victim.handle)).not.toBeNull();
    await apnsLog();
  });

  it("refreshes a handle on its own token even when that's full, and an old one too", async () => {
    const token = "c6".repeat(32);
    const mine = await register(token);
    for (let i = 1; i < HANDLES_PER_TOKEN; i++) await register(token);
    // Unused past its 90 days but not swept yet: its own refresh keeps it.
    await env.DB.prepare("UPDATE handles SET used_at = 1000 WHERE handle = ?1").bind(mine.handle).run();
    expect((await post("/v1/refresh", { ...mine, token, env: "production" }, address())).status).toBe(204);
    expect(await row(mine.handle)).toMatchObject({ token, sandbox: 0 });
  });
});

describe("push", () => {
  it("asks APNs for the neutral alert with the ciphertext, signed with the team's key", async () => {
    const { handle, secret } = await register(PHONE);
    const r = await post("/v1/push", { handle, secret, ciphertext: CIPHERTEXT, collapse: "3f9a0c" });
    expect(r.status).toBe(204);
    const [sent, ...more] = await apnsLog();
    expect(more).toEqual([]);
    expect(sent).toMatchObject({
      env: "sandbox",
      token: PHONE,
      jwtValid: true,
      topic: TOPIC,
      pushType: "alert",
      priority: "10",
      collapse: "3f9a0c",
    });
    expect(Number(sent.expiration)).toBeGreaterThan(Date.now() / 1000 + 23 * 3600);
    expect(sent.body).toBe(
      `{"aps":{"alert":{"title":"Flotilla","body":"New activity"},"mutable-content":1,"sound":"default"},"e":"${CIPHERTEXT}"}`,
    );
    // A production phone goes to production APNs; no collapse id, no header.
    const prod = await register("d4".repeat(32), { env: "production" });
    expect((await post("/v1/push", { ...prod, ciphertext: CIPHERTEXT })).status).toBe(204);
    const [p] = await apnsLog();
    expect(p).toMatchObject({ env: "production", token: "d4".repeat(32), collapse: null, jwtValid: true });
  });

  it("refuses a wrong secret or an unknown handle alike, and sends nothing", async () => {
    const { handle, secret } = await register();
    const wrong = secret.slice(0, -1) + (secret.endsWith("A") ? "B" : "A");
    for (const body of [
      { handle, secret: wrong, ciphertext: CIPHERTEXT },
      { handle: "A".repeat(22), secret, ciphertext: CIPHERTEXT },
    ]) {
      const r = await post("/v1/push", body);
      expect(r.status).toBe(404);
      expect(await r.json()).toEqual({ error: "No such handle", code: "no_such_handle" });
    }
    const r = await post("/v1/push", { handle, ciphertext: CIPHERTEXT });
    expect(r.status).toBe(400);
    expect(await apnsLog()).toEqual([]);
  });

  it("takes only base64 under its size limit, and a short collapse id", async () => {
    const { handle, secret } = await register();
    const ok = "A".repeat(MAX_CIPHERTEXT);
    expect((await post("/v1/push", { handle, secret, ciphertext: ok })).status).toBe(204);
    expect((await post("/v1/push", { handle, secret, ciphertext: ok + "AAAA" })).status).toBe(413);
    expect((await post("/v1/push", { handle, secret, ciphertext: "not base64!" })).status).toBe(400);
    expect((await post("/v1/push", { handle, secret, ciphertext: "AAA" })).status).toBe(400);
    expect((await post("/v1/push", { handle, secret, ciphertext: CIPHERTEXT, collapse: "x".repeat(65) })).status).toBe(400);
    expect((await post("/v1/push", { handle, secret, ciphertext: CIPHERTEXT, collapse: "a b" })).status).toBe(400);
    expect((await apnsLog()).length).toBe(1);
  });

  it("drops the handle when APNs says the phone is gone", async () => {
    for (const token of ["dead".padEnd(64, "0"), "bad0".padEnd(64, "1")]) {
      const { handle, secret } = await register(token);
      const r = await post("/v1/push", { handle, secret, ciphertext: CIPHERTEXT });
      expect(r.status).toBe(410);
      expect(await row(handle)).toBeNull();
      expect((await post("/v1/push", { handle, secret, ciphertext: CIPHERTEXT })).status).toBe(404);
    }
    expect((await apnsLog()).length).toBe(2);
    // The phone refreshed onto a new token while APNs was refusing the old one: the handle
    // stays, on its new token, and the server isn't told it's gone.
    const raced = await register("dead".padEnd(64, "2"));
    await apnsAnswers({ status: 410, reason: "Unregistered", delay: 400 });
    const pushing = post("/v1/push", { ...raced, ciphertext: CIPHERTEXT });
    await new Promise((r) => setTimeout(r, 100));
    expect((await post("/v1/refresh", { ...raced, token: "b3".repeat(32), env: "sandbox" }, address())).status).toBe(204);
    expect((await pushing).status).toBe(502);
    expect(await row(raced.handle)).toMatchObject({ token: "b3".repeat(32) });
    await apnsLog();
  });

  it("makes the provider token again once when APNs says it's stale", async () => {
    const { handle, secret } = await register();
    await post("/v1/push", { handle, secret, ciphertext: CIPHERTEXT });
    const [first] = await apnsLog();
    await apnsAnswers({ status: 403, reason: "ExpiredProviderToken" });
    expect((await post("/v1/push", { handle, secret, ciphertext: CIPHERTEXT })).status).toBe(204);
    const [stale, fresh] = await apnsLog();
    expect(stale.jwt).toBe(first.jwt);
    expect(fresh.jwt).not.toBe(first.jwt);
    expect(fresh.jwtValid).toBe(true);
    // The new one is shared through D1: another instance (its memory forgotten) takes it.
    forgetCachedToken();
    await post("/v1/push", { handle, secret, ciphertext: CIPHERTEXT });
    const [shared] = await apnsLog();
    expect(shared.jwt).toBe(fresh.jwt);
    // Refused twice: the server hears it, and the handle stays.
    await apnsAnswers({ status: 403, reason: "InvalidProviderToken" });
    await apnsAnswers({ status: 403, reason: "InvalidProviderToken" });
    expect((await post("/v1/push", { handle, secret, ciphertext: CIPHERTEXT })).status).toBe(502);
    expect(await row(handle)).not.toBeNull();
    // APNs' other refusals: the reason, and the handle stays.
    await apnsAnswers({ status: 400, reason: "PayloadTooLarge" });
    const r = await post("/v1/push", { handle, secret, ciphertext: CIPHERTEXT });
    expect(r.status).toBe(502);
    expect(await r.json()).toEqual({ error: "APNs refused it: 400 PayloadTooLarge" });
    await apnsAnswers({ status: 429, reason: "TooManyRequests" });
    expect((await post("/v1/push", { handle, secret, ciphertext: CIPHERTEXT })).status).toBe(429);
    expect(await row(handle)).not.toBeNull();
    await apnsLog();
  });

  it("says so when its key isn't one, and sends nothing", async () => {
    const { handle, secret } = await register();
    forgetCachedToken();
    await env.DB.prepare("DELETE FROM provider_token").run();
    const r = await worker.fetch(
      new Request("https://push.example.com/v1/push", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ handle, secret, ciphertext: CIPHERTEXT }),
      }),
      { ...env, APNS_KEY: "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----" },
    );
    expect(r.status).toBe(503);
    expect(await r.json()).toEqual({ error: "The relay's APNs key isn't usable" });
    expect(await apnsLog()).toEqual([]);
    forgetCachedToken();
  });

  it("says so when it has no key, and sends nothing", async () => {
    const { handle, secret } = await register();
    const r = await worker.fetch(
      new Request("https://push.example.com/v1/push", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ handle, secret, ciphertext: CIPHERTEXT }),
      }),
      { ...env, APNS_KEY: "" },
    );
    expect(r.status).toBe(503);
    expect(await apnsLog()).toEqual([]);
  });
});

describe("the provider token", () => {
  const stored = async () =>
    (await env.DB.prepare("SELECT token, issued_at FROM provider_token WHERE id = 1").first<{
      token: string;
      issued_at: number;
    }>())!;
  const clear = async () => {
    forgetCachedToken();
    await env.DB.prepare("DELETE FROM provider_token").run();
  };

  it("is one token for callers at once, and the one D1 keeps", async () => {
    await clear();
    const t = Math.floor(Date.now() / 1000);
    const tokens = await Promise.all(Array.from({ length: 12 }, () => providerToken(env, t)));
    expect(new Set(tokens).size).toBe(1);
    expect((await stored()).token).toBe(tokens[0]);
  });

  it("is the first published when instances race, at the start and at a rollover", async () => {
    await clear();
    const t = Math.floor(Date.now() / 1000);
    // Two instances made a token each from what they saw (nothing yet).
    const [a, b] = [await jwt(env, t), await jwt(env, t)];
    expect(a).not.toBe(b);
    expect((await publish(env, a, t, null)).token).toBe(a);
    expect((await publish(env, b, t, null)).token).toBe(a);
    // Both saw `a` grow old: the first replacement stands, the second takes it.
    const [c, d] = [await jwt(env, t + 2400), await jwt(env, t + 2400)];
    expect((await publish(env, c, t + 2400, a)).token).toBe(c);
    expect((await publish(env, d, t + 2400, a)).token).toBe(c);
    expect((await stored()).token).toBe(c);
  });

  it("isn't made again for a refusal another instance already answered", async () => {
    await clear();
    const t = Math.floor(Date.now() / 1000);
    const old = await providerToken(env, t);
    // Another instance replaced it meanwhile.
    const newer = await jwt(env, t + 1);
    expect((await publish(env, newer, t + 1, old)).token).toBe(newer);
    forgetCachedToken();
    // This instance's push with `old` was refused: it takes the newer one, makes nothing.
    expect(await providerToken(env, t + 2, old)).toBe(newer);
    expect((await stored()).token).toBe(newer);
    // The newer one refused too: now it's replaced.
    const fresh = await providerToken(env, t + 3, newer);
    expect(fresh).not.toBe(newer);
    expect((await stored()).token).toBe(fresh);
  });

  it("takes the shared token once when APNs says tokens change too often", async () => {
    await clear();
    const { handle, secret } = await register("e7".repeat(32));
    await apnsLog();
    await apnsAnswers({ status: 429, reason: "TooManyProviderTokenUpdates" });
    expect((await post("/v1/push", { handle, secret, ciphertext: CIPHERTEXT })).status).toBe(204);
    const [refused, again] = await apnsLog();
    expect(again.jwt).toBe((await stored()).token);
    expect(refused.jwt).toBe(again.jwt);
    // Twice running: the server hears 429 and tries later.
    await apnsAnswers({ status: 429, reason: "TooManyProviderTokenUpdates" });
    await apnsAnswers({ status: 429, reason: "TooManyProviderTokenUpdates" });
    const r = await post("/v1/push", { handle, secret, ciphertext: CIPHERTEXT });
    expect(r.status).toBe(429);
    await apnsLog();
  });
});

/// `n` requests in one go, from the start of a limit's window if one rolls over meanwhile
/// (the limits count in wall-clock minutes).
async function burst(n: number, request: (i: number) => Promise<Response>): Promise<number[]> {
  for (;;) {
    const minute = Math.floor(Date.now() / 60_000);
    const statuses: number[] = [];
    for (let i = 0; i < n; i++) statuses.push((await request(i)).status);
    if (Math.floor(Date.now() / 60_000) === minute) return statuses;
  }
}

describe("rate limits", () => {
  it("limits pushes per handle", async () => {
    const { handle, secret } = await register("e5".repeat(32));
    const statuses = await burst(31, () => post("/v1/push", { handle, secret, ciphertext: CIPHERTEXT }, address()));
    expect(statuses.filter((s) => s === 204).length).toBeLessThanOrEqual(30);
    expect(statuses.at(-1)).toBe(429);
    await apnsLog();
  });

  it("limits pushes per address, wrong secrets included", async () => {
    const from = address();
    const statuses = await burst(301, () =>
      post("/v1/push", { handle: "B".repeat(22), secret: "C".repeat(43), ciphertext: CIPHERTEXT }, from),
    );
    expect(statuses.slice(0, 300).every((s) => s === 404)).toBe(true);
    expect(statuses.at(-1)).toBe(429);
  });

  it("limits registering, refreshing and deleting per address", async () => {
    const from = address();
    const statuses = await burst(21, (i) =>
      post("/v1/register", { token: (i % 16).toString(16).repeat(2) + "f6".repeat(31), topic: TOPIC, env: "sandbox" }, from),
    );
    expect(statuses.slice(0, 20).every((s) => s === 201)).toBe(true);
    expect(statuses.at(-1)).toBe(429);
    const limited = await post("/v1/register", { token: PHONE, topic: TOPIC, env: "sandbox" }, from);
    expect(limited.headers.get("retry-after")).toBe("60");
    expect((await post("/v1/unregister", { handle: "A".repeat(22), secret: "A".repeat(43) }, from)).status).toBe(429);
  });
});

describe("addresses", () => {
  it("counts an IPv6 address by its /64, and an IPv4 one as it is", () => {
    expect(addressKey("203.0.113.9")).toBe("203.0.113.9");
    expect(addressKey("::ffff:203.0.113.9")).toBe("203.0.113.9");
    expect(addressKey("2001:db8:85a3:12:8a2e:370:7334:1")).toBe("2001:db8:85a3:12::/64");
    expect(addressKey("2001:DB8:85a3:0012::ff")).toBe("2001:db8:85a3:12::/64");
    expect(addressKey("2001:db8::1")).toBe("2001:db8:0:0::/64");
    expect(addressKey("::1")).toBe("0:0:0:0::/64");
  });

  it("limits a /64 as one address", async () => {
    const net = `2001:db8:${(ip + 4096).toString(16)}:1`;
    ip += 1;
    const statuses = await burst(21, (i) =>
      post("/v1/unregister", { handle: "A".repeat(22), secret: "A".repeat(43) }, `${net}::${(i + 1).toString(16)}`),
    );
    expect(statuses.slice(0, 20).every((s) => s === 404)).toBe(true);
    expect(statuses.at(-1)).toBe(429);
  });
});

describe("refresh and unregister", () => {
  it("moves a handle to the phone's new token", async () => {
    const { handle, secret } = await register();
    const next = "b2".repeat(32);
    expect((await post("/v1/refresh", { handle, secret, token: next, env: "production" })).status).toBe(204);
    expect(await row(handle)).toMatchObject({ token: next, sandbox: 0 });
    await post("/v1/push", { handle, secret, ciphertext: CIPHERTEXT });
    expect(await apnsLog()).toMatchObject([{ token: next, env: "production" }]);
    expect((await post("/v1/refresh", { handle, secret: "A".repeat(43), token: PHONE, env: "sandbox" })).status).toBe(
      404,
    );
  });

  it("forgets a handle: with its secret only, and then for good", async () => {
    const { handle, secret } = await register();
    expect((await post("/v1/unregister", { handle, secret: "A".repeat(43) })).status).toBe(404);
    expect(await row(handle)).not.toBeNull();
    expect((await post("/v1/unregister", { handle, secret })).status).toBe(204);
    expect(await row(handle)).toBeNull();
    expect((await post("/v1/unregister", { handle, secret })).status).toBe(404);
    expect((await post("/v1/push", { handle, secret, ciphertext: CIPHERTEXT })).status).toBe(404);
    expect(await apnsLog()).toEqual([]);
  });

  it("notes a handle's last use at most hourly", async () => {
    const { handle, secret } = await register("e8".repeat(32));
    const t = Math.floor(Date.now() / 1000);
    // Used half an hour ago: a push leaves it.
    await env.DB.prepare("UPDATE handles SET used_at = ?2 WHERE handle = ?1").bind(handle, t - 1800).run();
    expect((await post("/v1/push", { handle, secret, ciphertext: CIPHERTEXT })).status).toBe(204);
    expect(await row(handle)).toMatchObject({ used_at: t - 1800 });
    // An hour or more: a push notes it.
    await env.DB.prepare("UPDATE handles SET used_at = ?2 WHERE handle = ?1").bind(handle, t - 3600).run();
    expect((await post("/v1/push", { handle, secret, ciphertext: CIPHERTEXT })).status).toBe(204);
    expect(Number((await row(handle))!.used_at)).toBeGreaterThanOrEqual(t);
    await apnsLog();
  });

  it("drops handles unused for 90 days from the daily cron", async () => {
    const old = await register();
    const fresh = await register();
    await env.DB.prepare("UPDATE handles SET used_at = ?2 WHERE handle = ?1")
      .bind(old.handle, Math.floor(Date.now() / 1000) - UNUSED_SECS - 1)
      .run();
    await worker.scheduled(createScheduledController({ cron: "17 4 * * *" }), env);
    expect(await row(old.handle)).toBeNull();
    expect(await row(fresh.handle)).not.toBeNull();
  });

  it("drops handles unused for 90 days", async () => {
    const old = await register();
    const fresh = await register();
    const now = Math.floor(Date.now() / 1000);
    await env.DB.prepare("UPDATE handles SET used_at = ?2 WHERE handle = ?1")
      .bind(old.handle, now - UNUSED_SECS - 1)
      .run();
    expect(await sweep(env, now)).toBeGreaterThanOrEqual(1);
    expect(await row(old.handle)).toBeNull();
    expect(await row(fresh.handle)).not.toBeNull();
  });
});

describe("the self-test", () => {
  const selftest = (query = "", token = "selftest-token-for-tests") =>
    SELF.fetch(`https://push.example.com/v1/selftest${query}`, { headers: { authorization: `Bearer ${token}` } });

  it("is only for whoever has its token, and guesses are limited", async () => {
    expect((await selftest("", "wrong")).status).toBe(403);
    const from = address();
    const guesses = await burst(21, () =>
      SELF.fetch("https://push.example.com/v1/selftest", {
        headers: { authorization: "Bearer wrong", "cf-connecting-ip": from },
      }),
    );
    expect(guesses.slice(0, 20).every((s) => s === 403)).toBe(true);
    expect(guesses.at(-1)).toBe(429);
    const off = await worker.fetch(new Request("https://push.example.com/v1/selftest"), { ...env, SELFTEST_TOKEN: "" });
    expect(off.status).toBe(404);
    expect(await apnsLog()).toEqual([]);
  });

  it("passes only on APNs' own JSON answer", async () => {
    // No key: APNs refuses the dummy provider token, which it can only say over HTTP/2.
    const dummy = await selftest("?dummy");
    expect(dummy.status).toBe(200);
    expect(await dummy.json()).toMatchObject({
      ok: true,
      status: 403,
      reason: "InvalidProviderToken",
      apns_id: true,
      key: "none",
    });
    const [sent] = await apnsLog();
    expect(sent).toMatchObject({ env: "sandbox", token: "0".repeat(64), jwt: DUMMY_JWT, jwtValid: false });
    // The relay's key: past the provider token, to the made-up device token.
    expect(await (await selftest()).json()).toMatchObject({ ok: true, reason: "BadDeviceToken", key: "accepted" });
    expect(await (await selftest("?env=production")).json()).toMatchObject({ ok: true, key: "accepted" });
    expect((await apnsLog()).map((s) => s.env)).toEqual(["sandbox", "production"]);
    // Anything else in the way (an HTTP/1.1 refusal, a proxy's page) fails.
    await apnsAnswers({ status: 502, raw: "<html>Bad gateway</html>" });
    const failed = await selftest("?dummy");
    expect(failed.status).toBe(502);
    expect(await failed.json()).toMatchObject({ ok: false, status: 502, reason: null });
    await apnsLog();
  });
});

describe("routes", () => {
  it("answers at / and nowhere it doesn't serve", async () => {
    const root = await SELF.fetch("https://push.example.com/");
    expect(await root.text()).toBe("Flotilla push relay\n");
    expect((await SELF.fetch("https://push.example.com/v1/push")).status).toBe(404);
    expect((await SELF.fetch("https://push.example.com/v1/handles/x", { method: "DELETE" })).status).toBe(404);
  });
});
