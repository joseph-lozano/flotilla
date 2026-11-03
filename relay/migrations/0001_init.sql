-- What the relay keeps: one row per handle a phone registered, and the token it signs APNs
-- requests with. Nothing else about a phone or whoever has it.
CREATE TABLE handles (
  -- Random, given to the phone at registration (base64url, 16 bytes).
  handle TEXT PRIMARY KEY,
  -- SHA-256 of the handle's secret, in hex. The secret itself is never kept.
  secret_hash TEXT NOT NULL,
  -- The phone's APNs device token (hex), its app's bundle id, and which APNs.
  token TEXT NOT NULL,
  topic TEXT NOT NULL,
  sandbox INTEGER NOT NULL,
  -- Unix seconds. A handle unused for 90 days is dropped.
  created_at INTEGER NOT NULL,
  used_at INTEGER NOT NULL
);
CREATE INDEX handles_token ON handles (token);
CREATE INDEX handles_used_at ON handles (used_at);

-- The APNs provider token (a JWT), shared by the Worker's instances: Apple refuses one made
-- more often than every 20 minutes.
CREATE TABLE provider_token (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  token TEXT NOT NULL,
  issued_at INTEGER NOT NULL
);
