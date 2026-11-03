// A fake APNs for the relay's tests and the local end-to-end run (script/e2e).
//
// - POST [/sandbox|/production]/3/device/<token>: answers as APNs would, after checking the
//   provider token's ES256 signature against the test key's public key. A device token
//   starting "dead" is Unregistered (410), one starting "bad0" is BadDeviceToken (400), and
//   an answer queued with POST /_answer goes first (after its `delay` in ms, if it has one).
// - GET /_log: what it was sent since the last look (and forgets it).
// - With FAKE_APNS_OUT=<dir>, each notification's body is also written there, for
//   `xcrun simctl push`.
//
// Run alone: node test/fake-apns.mjs [port]

import { createServer } from "node:http";
import { createPublicKey, verify, randomUUID } from "node:crypto";
import { writeFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

/// The P-256 key the Rust tests sign with too (crates/flotilla-core/src/push.rs), made for
/// tests only: key id ABC123DEFG, team DEF123GHIJ.
export const TEST_P8 = `-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg6z48n4GLbR6j/1dc
2OyUD5ox0tSKqT5xA6JRnsjgB6GhRANCAASrhjQXMLZ4MEuZ5F9lDX0ccqYB+33c
pv0zsscQHkNwBtHmaIeo4wdam38MPEW9MsTfVDD5y0wDXYO99o889Uvj
-----END PRIVATE KEY-----
`;
export const TEST_KEY_ID = "ABC123DEFG";
export const TEST_TEAM_ID = "DEF123GHIJ";

function jwtValid(authorization, publicKey) {
  const m = /^bearer (.+)$/.exec(authorization ?? "");
  if (!m) return false;
  const parts = m[1].split(".");
  if (parts.length !== 3) return false;
  try {
    const header = JSON.parse(Buffer.from(parts[0], "base64url").toString());
    const claims = JSON.parse(Buffer.from(parts[1], "base64url").toString());
    if (header.alg !== "ES256" || header.kid !== TEST_KEY_ID || claims.iss !== TEST_TEAM_ID) return false;
    return verify(
      "sha256",
      Buffer.from(`${parts[0]}.${parts[1]}`),
      { key: publicKey, dsaEncoding: "ieee-p1363" },
      Buffer.from(parts[2], "base64url"),
    );
  } catch {
    return false;
  }
}

export function start(port = 0, out = process.env.FAKE_APNS_OUT) {
  const publicKey = createPublicKey(TEST_P8);
  let log = [];
  const answers = [];
  let n = 0;
  if (out) mkdirSync(out, { recursive: true });
  const server = createServer((req, res) => {
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => {
      const text = Buffer.concat(chunks).toString();
      const url = new URL(req.url, "http://fake");
      const reply = (status, value, headers = {}) => {
        res.writeHead(status, { "content-type": "application/json", ...headers });
        res.end(value === undefined ? "" : JSON.stringify(value));
      };
      if (req.method === "GET" && url.pathname === "/_log") {
        const seen = log;
        log = [];
        return reply(200, seen);
      }
      if (req.method === "POST" && url.pathname === "/_answer") {
        answers.push(JSON.parse(text));
        return reply(204);
      }
      const m = /^(?:\/(sandbox|production))?\/3\/device\/([0-9a-f]+)$/.exec(url.pathname);
      if (req.method !== "POST" || !m) return reply(404, { reason: "BadPath" });
      const token = m[2];
      const valid = jwtValid(req.headers.authorization, publicKey);
      log.push({
        env: m[1] ?? "any",
        token,
        jwt: (req.headers.authorization ?? "").replace(/^bearer /, ""),
        jwtValid: valid,
        topic: req.headers["apns-topic"],
        pushType: req.headers["apns-push-type"],
        priority: req.headers["apns-priority"],
        expiration: req.headers["apns-expiration"],
        collapse: req.headers["apns-collapse-id"] ?? null,
        body: text,
      });
      const id = randomUUID().toUpperCase();
      if (answers[0]?.delay) {
        const { delay, ...rest } = answers.shift();
        answers.unshift(rest);
        setTimeout(() => answer(), delay);
        return;
      }
      answer();
      function answer() {
      const queued = answers.shift();
      if (queued) {
        if (queued.raw !== undefined) {
          res.writeHead(queued.status, { "content-type": "text/html" });
          return res.end(queued.raw);
        }
        return reply(queued.status, queued.reason ? { reason: queued.reason } : undefined, { "apns-id": id });
      }
      if (!valid) return reply(403, { reason: "InvalidProviderToken" }, { "apns-id": id });
      if (token.startsWith("dead")) return reply(410, { reason: "Unregistered", timestamp: Date.now() }, { "apns-id": id });
      if (token.startsWith("bad0") || /^0+$/.test(token)) return reply(400, { reason: "BadDeviceToken" }, { "apns-id": id });
      if (out) writeFileSync(join(out, `push-${String(++n).padStart(3, "0")}.json`), text);
      return reply(200, undefined, { "apns-id": id });
      }
    });
  });
  return new Promise((resolve) => server.listen(port, "127.0.0.1", () => resolve(server)));
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const server = await start(Number(process.argv[2] ?? 0));
  console.log(`fake APNs on http://127.0.0.1:${server.address().port}`);
}
