// Writes test/vectors.json: what a server's encryption must give, made with Node's own
// ChaCha20-Poly1305 and HMAC so the Rust (ring) and Swift (CryptoKit) code is checked against
// a third implementation. Run: node script/make-vectors.mjs
import { createCipheriv, createHmac } from "node:crypto";
import { writeFileSync } from "node:fs";

const hex = (s) => Buffer.from(s, "hex");
const seal = (key, nonce, plaintext) => {
  const c = createCipheriv("chacha20-poly1305", key, nonce, { authTagLength: 16 });
  const ct = Buffer.concat([c.update(plaintext, "utf8"), c.final()]);
  return Buffer.concat([nonce, ct, c.getAuthTag()]).toString("base64");
};
const collapse = (key, id) => createHmac("sha256", key).update(`collapse:${id}`).digest("hex").slice(0, 32);

const key = hex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f");
const other = hex("1f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403020100");
const plaintexts = [
  `{"aps":{"alert":{"title":"Fix flaky retry test","body":"Finished. PR #42 is open and its 12 checks pass."},"sound":"default","thread-id":"t1"},"flotilla":{"sent_at":1793664000,"server":"srv_3f5c1a7e92d4","kind":"turn_finished","thread":"t1"}}`,
  `{"aps":{"alert":{"title":"Café ☕ «naïve» 日本語 🚢","body":"Asks: Which file name should I create?"},"sound":"default","thread-id":"t2"},"flotilla":{"sent_at":1793664000,"server":"srv_3f5c1a7e92d4","kind":"asked","thread":"t2"}}`,
  `{"aps":{"alert":{"title":"web","body":"Setup failed on main@9f3c2ab."},"sound":"default","thread-id":"p1"},"flotilla":{"sent_at":1793664000,"server":"srv_3f5c1a7e92d4","kind":"setup_failed"}}`,
];
const vectors = {
  about:
    "Flotilla's push encryption: ChaCha20-Poly1305, a 32-byte key, a random 12-byte nonce, no associated data. The ciphertext is base64 (standard, padded) of nonce || ciphertext || tag: CryptoKit's ChaChaPoly.SealedBox.combined. The plaintext is the server's APNs payload with flotilla.sent_at (Unix seconds) added. The collapse id is the first 32 hex digits of HMAC-SHA256(key, \"collapse:\" + id). Made by relay/script/make-vectors.mjs.",
  key: key.toString("base64"),
  other_key: other.toString("base64"),
  seals: plaintexts.map((plaintext, i) => {
    const nonce = Buffer.alloc(12, 0);
    nonce.writeUInt32BE(i + 1, 8);
    return { nonce: nonce.toString("hex"), plaintext, ciphertext: seal(key, nonce, plaintext) };
  }),
  collapses: [
    { id: "turn_finished:t1", collapse: collapse(key, "turn_finished:t1") },
    { id: "setup_failed:p1", collapse: collapse(key, "setup_failed:p1") },
  ],
};
writeFileSync(new URL("../test/vectors.json", import.meta.url), JSON.stringify(vectors, null, 2) + "\n");
