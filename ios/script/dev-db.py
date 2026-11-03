#!/usr/bin/env python3
"""Make a database for running the iPhone app against `flotilla serve --offline`.

    python3 -I ios/script/dev-db.py <data dir> [--pr] [--states] [--project] [--fix] [--delegate] [--usage | --usage-tight] [--agent-usage] [--stress [turns]] [--ask [answered]] [--services [--public]] [--empty]

It copies /tmp/side-live4/db (a thread with two hand-off cards and an "agents answered"
prompt), adds the thread from /tmp/side-live2/db, unread, and the threads from
/tmp/flotilla-subagents/demo.db (Claude Code, Codex and OpenCode subagents), all real
recordings. Neither has a pull request, so `--pr` gives side-live2's thread a fixture one, open
and ready to merge, for the PR bar and Hold to merge, and `--states` stops every VM but one, which
has half an hour left. `--project` adds a GitHub project (flotilla-playground) with the median
thread in it, `--fix` gives the Codex subagents thread a pull request with failing checks, a
conflict and changes requested (Fix, Resolve, Address review), and `--delegate` turns the Codex
review on the slug thread into a delegate with a thread of its own (Open thread). `--usage`
keeps the account's usage as last read from boat (what an offline server shows), comfortable,
and `--usage-tight` near the limits: the last minute's starts used up, the 24 hours' and the
plan's VM hours nearly, extra starts on. `--agent-usage` keeps 30 days of what the agents spent
(made up, hour by hour, as `flotilla_core::spend` keeps it: Claude Code, Codex, and OpenCode on a
model with no known price), both accounts' limits, and the model prices beside the database
(the recorded subset in crates/flotilla-core/testdata/prices). `--stress` adds a long thread ("Stress: a long
thread", 220 turns by default, about 3,100 messages) made of boat-shaped events, with huge tool
output every tenth turn (a 330 KB Bash output, a 2,000-line Read, a 180 KB Write, multibyte text
across the 32 KB cut), for paging and capping. `--ask` adds a Codex thread ("Pick a file name")
whose agent asked a question with request_user_input_async and waits on the answer (a live
turn's events and the question as `flotilla_core::asks` keeps it), unread; `--ask answered` has
it answered (beta.txt) and the agent's next turn. `--services` gives the median thread services
as `flotilla_core::services` keeps them (`web` ready with a preview and a second link, `worker`
stopped, `api` exited with what it printed), the OpenCode thread a services file with a problem,
and leaves the slug thread up without services and the Claude Code subagents thread stopped, for
Preview's sheet, its port field and a VM it wakes; with `--public`, the median thread's `web` is
public for the next hour and its port 5173 (no service) until stopped, as `flotilla_core::public`
keeps them. `--empty` then leaves no threads (the models,
environments and anything else kept stay), as on a server just set up. The sources are opened
read-only. What they say of the real machines and accounts they were recorded on doesn't come
along: a VM's hostname reads as a stand-in (`box`), and the GitHub repo list kept for Add project
(under its owner's login) is left out.
"""
import os
import re
import sqlite3
import sys

BASE = "/tmp/side-live4/db"
ADD = [("/tmp/side-live2/db", True), ("/tmp/flotilla-subagents/demo.db", False)]
TABLES = ["threads", "events", "turns", "sent_prompts", "sync_state", "thread_views", "thread_archive", "summaries", "messages", "thread_prs"]


def columns(db, schema, table):
    return [r[1] for r in db.execute(f"PRAGMA {schema}.table_info({table})")]


def main():
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    path = os.path.join(out, "flotilla.db")
    for suffix in ("", "-wal", "-shm", ".lock"):
        if os.path.exists(path + suffix):
            os.remove(path + suffix)
    src = sqlite3.connect(f"file:{BASE}?mode=ro", uri=True)
    db = sqlite3.connect(path)
    src.backup(db)
    src.close()
    for i, (other, unread) in enumerate(ADD):
        if not os.path.exists(other):
            print(f"skipping {other}: not there")
            continue
        db.execute(f"ATTACH DATABASE 'file:{other}?mode=ro' AS o{i}", ())
        ids = [r[0] for r in db.execute(f"SELECT id FROM o{i}.threads")]
        for table in TABLES:
            mine, theirs = columns(db, "main", table), columns(db, f"o{i}", table)
            if not theirs:
                continue
            shared = [c for c in mine if c in theirs]
            key = "id" if table == "threads" else "thread_id"
            cols = ", ".join(shared)
            db.execute(f"INSERT OR REPLACE INTO main.{table} ({cols}) SELECT {cols} FROM o{i}.{table} WHERE {key} IN ({','.join('?' * len(ids))})", ids)
        for t in ids:
            db.execute(f"INSERT OR REPLACE INTO main.kv SELECT * FROM o{i}.kv WHERE key LIKE ?", (f"%{t}%",))
            if unread:
                db.execute("INSERT OR REPLACE INTO thread_views (thread_id, viewed_at) VALUES (?, '2026-01-01T00:00:00Z')", (t,))
        db.commit()
        db.execute(f"DETACH DATABASE o{i}")
        print(f"added {len(ids)} thread(s) from {other}")
    scrub(db)
    if "--pr" in sys.argv:
        t = db.execute("SELECT id FROM threads WHERE title LIKE 'Median%'").fetchone()
        if t:
            checks = '[' + ','.join(f'{{"name":"{n}","state":"pass","url":null}}' for n in ["build", "test (ubuntu)", "test (macos)", "clippy", "fmt", "docs", "audit", "msrv", "e2e", "coverage", "bench", "lint"]) + ']'
            db.execute(
                "INSERT OR REPLACE INTO thread_prs (thread_id, number, url, title, state, is_draft, mergeable, review, checks_json, additions, deletions, linked_by, checked_at, head, base, updated_at) "
                "VALUES (?, 42, 'https://github.com/acme/flotilla-playground/pull/42', 'Fix the even-length median', 'OPEN', 0, 'MERGEABLE', 'APPROVED', ?, 48, 12, 'auto', '2026-10-31T13:00:00Z', 'flot/fix-median', 'main', '2026-10-31T13:00:00Z')",
                (t[0], checks),
            )
            db.execute("UPDATE threads SET branch='flot/fix-median' WHERE id=?", (t[0],))
            db.commit()
            print("gave the median thread a fixture pull request (#42)")
    if "--project" in sys.argv or "--fix" in sys.argv:
        project(db)
    if "--fix" in sys.argv:
        fix(db)
    if "--delegate" in sys.argv:
        delegate(db)
    if "--states" in sys.argv:
        vary(db)
    if "--usage" in sys.argv or "--usage-tight" in sys.argv:
        usage(db, "--usage-tight" in sys.argv)
    if "--agent-usage" in sys.argv:
        agent_usage(db, out)
    if "--stress" in sys.argv:
        at = sys.argv.index("--stress")
        turns = int(sys.argv[at + 1]) if at + 1 < len(sys.argv) and sys.argv[at + 1].isdigit() else 220
        stress(db, turns)
    if "--ask" in sys.argv:
        at = sys.argv.index("--ask")
        ask(db, at + 1 < len(sys.argv) and sys.argv[at + 1] == "answered")
    if "--services" in sys.argv:
        services(db)
        if "--public" in sys.argv:
            public(db)
    if "--empty" in sys.argv:
        empty(db)
    # What the scrub missed, anywhere: no database is left to use.
    left = leaks(db)
    if left:
        db.close()
        for suffix in ("", "-wal", "-shm"):
            if os.path.exists(path + suffix):
                os.remove(path + suffix)
        sys.exit(f"a real machine's name is still in {', '.join(f'{t}.{c} ({n})' for t, c, n in left)}: add a stand-in for it")
    # Nothing left out stays in the file's free pages either.
    db.commit()
    db.execute("VACUUM")
    print(f"threads: {db.execute('SELECT count(*) FROM threads').fetchone()[0]} in {path}")


# What the recordings may say about real machines, and what each reads as instead: boat VM
# hostnames, any tailnet's name, and the machine names listed (one a line) in
# ~/.config/flotilla-dev/scrub or $DEV_DB_SCRUB (comma-separated), which stay out of the repo.
def _names():
    path = os.path.expanduser("~/.config/flotilla-dev/scrub")
    listed = open(path).read().splitlines() if os.path.exists(path) else []
    listed += os.environ.get("DEV_DB_SCRUB", "").split(",")
    return [n.strip() for n in listed if n.strip()]


SCRUBBED_NAMES = _names()
STAND_INS = [
    (re.compile(r"box-node-[0-9a-f]{6,}"), "box"),
    (re.compile(r"(?<![a-z0-9-])[a-z0-9-]+\.ts\.net", re.I), "tail1234.ts.net"),
] + [(re.compile(re.escape(n)), "box") for n in SCRUBBED_NAMES]


def scrub(db):
    """The recordings' VM hostnames as stand-ins, in every text a thread keeps (its events, which
    its messages are rebuilt from, and what's kept beside them), and no GitHub repo list, paired
    devices or pairing codes."""
    db.create_function("scrub", 1, lambda v: v if not isinstance(v, str) else _scrubbed(v), deterministic=True)
    like = " OR ".join(f"{{c}} LIKE '%{w}%'" for w in ["box-node-", ".ts.net"] + [n.replace("'", "''") for n in SCRUBBED_NAMES])
    n = 0
    for table in TABLES + ["kv"]:
        for c in [r[1] for r in db.execute(f"PRAGMA table_info({table})") if r[2].upper() in ("TEXT", "")]:
            n += db.execute(f"UPDATE {table} SET {c} = scrub({c}) WHERE {like.format(c=c)}").rowcount
    db.execute("DELETE FROM kv WHERE key LIKE 'repo_cache:%'")
    for table in ("devices", "pairing_codes"):
        if db.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", (table,)).fetchone():
            db.execute(f"DELETE FROM {table}")
    db.commit()
    if n:
        print(f"gave {n} VM hostname(s) a stand-in")


def leaks(db):
    """Each table and text column where a real machine's name is still there (the scrub would
    change it), with how many rows."""
    db.create_function("leaky", 1, lambda v: isinstance(v, str) and _scrubbed(v) != v, deterministic=True)
    out = []
    for (table,) in db.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name").fetchall():
        for c in [r[1] for r in db.execute(f'PRAGMA table_info("{table}")') if r[2].upper() in ("TEXT", "")]:
            n = db.execute(f'SELECT count(*) FROM "{table}" WHERE leaky("{c}")').fetchone()[0]
            if n:
                out.append((table, c, n))
    return out


def _scrubbed(v):
    for pattern, stand_in in STAND_INS:
        v = pattern.sub(stand_in, v)
    return v


def empty(db):
    """No threads, nor anything kept about one."""
    ids = [r[0] for r in db.execute("SELECT id FROM threads")]
    for table in TABLES:
        key = "id" if table == "threads" else "thread_id"
        db.execute(f"DELETE FROM {table} WHERE {key} IN ({','.join('?' * len(ids))})", ids)
    for t in ids:
        db.execute("DELETE FROM kv WHERE key LIKE ?", (f"%{t}%",))
    db.commit()
    print(f"removed every thread ({len(ids)})")


PROJECT = "0bc02864-c677-412c-821e-f5d99d6cc39d"
SLUG = "31360c1e-cbab-4b44-993c-7dec50d661f5"
CODEX_REVIEW = "0af928b2-5236-41ce-9310-6b0d65a4acc4"


def project(db):
    db.execute(
        "INSERT OR REPLACE INTO projects (id, owner, repo, default_branch, environment_name, created_at) "
        "VALUES (?, 'acme', 'flotilla-playground', 'main', 'flotilla-playground-cd084e', '2026-10-30T15:53:48.332Z')",
        (PROJECT,),
    )
    db.execute("UPDATE threads SET repo_project_id=?, branch=COALESCE(branch, 'flot/fix-median') WHERE title LIKE 'Median%'", (PROJECT,))
    db.commit()
    print("added the flotilla-playground project")


def fix(db):
    """A pull request that needs the agent: failing checks, a conflict, changes requested."""
    t = db.execute("SELECT id FROM threads WHERE provider='codex' ORDER BY created_at LIMIT 1").fetchone()
    if not t:
        return
    checks = ('[{"name":"test (ubuntu)","state":"fail","url":"https://github.com/acme/flotilla-playground/actions/runs/7","description":"2 tests failed"},'
              '{"name":"build","state":"pass","url":null},{"name":"lint","state":"pass","url":null}]')
    db.execute(
        "INSERT OR REPLACE INTO thread_prs (thread_id, number, url, title, state, is_draft, mergeable, review, checks_json, additions, deletions, linked_by, checked_at, head, base, updated_at) "
        "VALUES (?, 43, 'https://github.com/acme/flotilla-playground/pull/43', 'Check disk space in parallel', 'OPEN', 0, 'CONFLICTING', 'CHANGES_REQUESTED', ?, 31, 4, 'auto', '2026-10-31T13:00:00Z', 'flot/parallel-checks', 'main', '2026-10-31T13:00:00Z')",
        (t[0], checks),
    )
    db.execute("UPDATE threads SET repo_project_id=?, branch='flot/parallel-checks' WHERE id=?", (PROJECT, t[0]))
    db.commit()
    print("gave the Codex thread a pull request that needs fixing (#43)")


def delegate(db):
    """The slug thread's Codex review as a delegate: a thread of its own, handed its task."""
    import json
    row = db.execute("SELECT value FROM kv WHERE key=?", (f"side:{SLUG}:{CODEX_REVIEW}",)).fetchone()
    if not row:
        return
    side = json.loads(row[0])
    db.execute("DELETE FROM kv WHERE key=?", (f"side:{SLUG}:{CODEX_REVIEW}",))
    db.execute(
        "INSERT OR REPLACE INTO threads (id, environment_id, environment_name, title, sandbox_id, conversation_id, provider, model, effort, fast, state, keep_running, created_at, updated_at) "
        "VALUES (?, '585998ba-4b24-47db-ac65-904eceb8cffa', 'base', ?, 'bx_codexrev', 'c_codexrev', 'codex', ?, 'medium', 0, 'stopped', 0, '2026-10-31T13:18:00.000Z', '2026-10-31T13:18:40.000Z')",
        (CODEX_REVIEW, side["title"], side["model"]),
    )
    start = side.get("started_ms") or 1793452680000
    end = side.get("ended_ms") or start + 30000
    task = side["task"] + "\n\n(Another agent handed you this task; your final message goes back to it.)"
    # Its task and answer as boat-shaped events (Codex's), as `--stress` writes its thread:
    # messages are derived, and the server rebuilds them from the events when it opens the
    # database.
    conv, turn = "c_codexrev", "task-codexrev"
    for table in ("events", "turns", "messages"):
        db.execute(f"DELETE FROM {table} WHERE thread_id=?", (CODEX_REVIEW,))
    events = [
        ("prompt", turn, "ev-codexrev-task", start, {"prompt": task, "status": "finished", "is_reverted": False}),
        ("response", "ev-codexrev-answer", "ev-codexrev-answer", end, {"content": side.get("answer") or "Reviewed.", "model": side["model"], "is_reverted": False}),
    ]
    for kind, key, eid, ts, data in events:
        raw = {"conversationId": conv, "data": data, "id": eid, "taskId": turn, "timestamp": ts, "type": kind}
        db.execute("INSERT INTO events (thread_id, key, event_id, task_id, type, timestamp, raw_json, settled, received_at) VALUES (?,?,?,?,?,?,?,1,'2026-10-31T13:18:40.000Z')",
                   (CODEX_REVIEW, key, eid, turn, kind, ts, json.dumps(raw)))
    db.execute("INSERT INTO turns (thread_id, prompt_id, status, done, settled, done_at, created_at) VALUES (?, ?, 'finished', 1, 1, '2026-10-31T13:18:40.000Z', '2026-10-31T13:18:00.000Z')", (CODEX_REVIEW, turn))
    db.execute("DELETE FROM kv WHERE key='messages_version'")
    db.execute("INSERT OR REPLACE INTO kv (key, value, updated_at) VALUES (?, ?, ?)", (f"parent:{CODEX_REVIEW}", SLUG, "2026-10-31T13:18:00.000Z"))
    db.commit()
    print("made the Codex review a delegate with its own thread")


def usage(db, tight):
    """The usage the server last read from boat (kv `usage`, as `flotilla_proto::Usage`)."""
    import datetime
    import json
    now = datetime.datetime.now(datetime.timezone.utc)
    ms = int(now.timestamp() * 1000)
    frees = (now + datetime.timedelta(seconds=40)).strftime("%Y-%m-%dT%H:%M:%S.000Z")
    u = {
        "vms": [3, 100],
        "minute": [12 if tight else 0, 12],
        "hour": [22 if tight else 7, 60],
        "day": [187 if tight else 120, 200],
        "vm_hours": [48 if tight else 549, 556],
        "renews": "2026-11-30T01:01:38.000Z",
        "blocked": None,
        "starts_free": frees if tight else None,
        "extra_starts": {"on": tight, "used": 7 if tight else 0, "spend_cents": [29 if tight else 0, 8000], "cap_reached": False},
        "read_ms": ms - 20_000,
    }
    db.execute("INSERT OR REPLACE INTO kv (key, value, updated_at) VALUES ('usage', ?, ?)", (json.dumps(u), now.strftime("%Y-%m-%dT%H:%M:%S.000Z")))
    db.commit()
    print("kept the account's usage" + (", near its limits" if tight else ""))


def agent_usage(db, out):
    """What the agents spent over the last 30 days (`agent_usage`, migration 13's table, which
    migration 16 makes again with a context tier and hour-long cache writes: both left at 0
    here), their accounts' limits (kv `limits:*`), and the model prices beside the database."""
    import datetime
    import json
    import math
    now = datetime.datetime.now(datetime.timezone.utc)
    ms = int(now.timestamp() * 1000)
    hour = 3_600_000
    db.executescript("""
    CREATE TABLE IF NOT EXISTS agent_usage (
      hour INTEGER NOT NULL, provider TEXT NOT NULL, model TEXT NOT NULL, speed TEXT NOT NULL, reported INTEGER NOT NULL,
      input INTEGER NOT NULL, cache_read INTEGER NOT NULL, cache_write INTEGER NOT NULL,
      output INTEGER NOT NULL, reasoning INTEGER NOT NULL, requests INTEGER NOT NULL, reported_usd REAL NOT NULL,
      PRIMARY KEY (hour, provider, model, speed, reported));
    CREATE TABLE IF NOT EXISTS agent_usage_seen (key INTEGER PRIMARY KEY, at INTEGER NOT NULL);
    CREATE INDEX IF NOT EXISTS agent_usage_seen_at ON agent_usage_seen (at);
    DELETE FROM agent_usage;
    """)
    # (harness, model, speed, days it ran in 30, tokens a day: input, cache read, cache write, output, reasoning)
    models = [
        ("claude-code", "claude-opus-5-5", "standard", 22, (600_000, 18_000_000, 900_000, 180_000, 0)),
        ("claude-code", "claude-opus-5-5", "fast", 4, (200_000, 6_000_000, 300_000, 60_000, 0)),
        ("claude-code", "claude-haiku-5-5", "standard", 26, (100_000, 2_000_000, 100_000, 30_000, 0)),
        ("codex", "gpt-6.1-sol", "standard", 18, (1_200_000, 9_000_000, 0, 120_000, 50_000)),
        ("codex", "gpt-6-luna", "standard", 12, (300_000, 1_000_000, 0, 20_000, 5_000)),
        ("opencode", "big-pickle", "standard", 5, (500_000, 3_000_000, 0, 50_000, 10_000)),
    ]
    rows = 0
    for i, (provider, model, speed, days, daily) in enumerate(models):
        for d in range(30):
            if (d * 7 + i * 3) % 30 >= days:
                continue
            swing = 0.55 + 0.45 * (1 + math.sin(d * 0.9 + i)) / 2
            start = ms - ms % hour - d * 24 * hour
            for h in (2, 5, 9):
                share = swing / 3
                t = [int(n * share) for n in daily]
                db.execute(
                    "INSERT OR REPLACE INTO agent_usage (hour, provider, model, speed, reported, input, cache_read, cache_write, output, reasoning, requests, reported_usd) "
                    "VALUES (?, ?, ?, ?, 0, ?, ?, ?, ?, ?, ?, 0)",
                    (start - h * hour, provider, model, speed, t[0], t[1], t[2], t[3], t[4], max(1, t[3] // 600)),
                )
                rows += 1
    iso = lambda dt: dt.strftime("%Y-%m-%dT%H:%M:%SZ")
    limits = {
        "claude": {"account": "claude", "plan": None, "read_ms": ms - 90_000, "windows": [
            {"id": "five_hour", "label": "5-hour", "used": 41, "resets_at": iso(now + datetime.timedelta(hours=2)), "minutes": 300, "scope": None},
            {"id": "seven_day", "label": "Weekly", "used": 62, "resets_at": iso(now + datetime.timedelta(days=3, hours=4)), "minutes": 10080, "scope": None},
            {"id": "seven_day_fable", "label": "Weekly · Fable", "used": 18, "resets_at": iso(now + datetime.timedelta(days=3, hours=4)), "minutes": 10080, "scope": "Fable"},
        ]},
        "chatgpt": {"account": "chatgpt", "plan": "pro", "read_ms": ms - 240_000, "windows": [
            {"id": "five_hour", "label": "5-hour", "used": 12, "resets_at": iso(now + datetime.timedelta(hours=3)), "minutes": 300, "scope": None},
            {"id": "seven_day", "label": "Weekly", "used": 23, "resets_at": iso(now + datetime.timedelta(days=5, hours=2)), "minutes": 10080, "scope": None},
        ]},
    }
    stamp = now.strftime("%Y-%m-%dT%H:%M:%S.000Z")
    for name, l in limits.items():
        db.execute("INSERT OR REPLACE INTO kv (key, value, updated_at) VALUES (?, ?, ?)", (f"limits:{name}", json.dumps(l), stamp))
    db.execute("INSERT OR REPLACE INTO kv (key, value, updated_at) VALUES ('spend:read_ms', ?, ?)", (str(ms - 120_000), stamp))
    db.commit()
    here = os.path.dirname(os.path.abspath(__file__))
    with open(os.path.join(here, "..", "..", "crates", "flotilla-core", "testdata", "prices", "litellm.json")) as f:
        prices = json.load(f)
    with open(os.path.join(out, "model-prices.json"), "w") as f:
        json.dump({"fetched_ms": ms - 3_600_000, "document": prices}, f)
    print(f"kept 30 days of what the agents spent ({rows} hours), their limits and the model prices")


def vary(db):
    """VMs as the mockups show them: one up with time left, the rest stopped."""
    import datetime
    now = datetime.datetime.now(datetime.timezone.utc)
    iso = lambda d: d.strftime("%Y-%m-%dT%H:%M:%S.000Z")
    db.execute("UPDATE threads SET state='stopped', last_stopped_at=?, archive_after=NULL WHERE id != '31360c1e-cbab-4b44-993c-7dec50d661f5'", (iso(now - datetime.timedelta(hours=2)),))
    db.execute("UPDATE threads SET state='idle', archive_after=? WHERE id = '31360c1e-cbab-4b44-993c-7dec50d661f5'", (iso(now + datetime.timedelta(minutes=30)),))
    db.commit()
    print("varied the VMs' states")


STRESS = "5a5a5a5a-0000-4000-8000-00000000c0de"
MEDIAN = "3312c93d-97e5-483e-b800-79c5df5c8aec"
UNAME = "f049a40a-ef06-48b1-a1ad-caedbc018d93"
CHECK = "6bbd614f-ef8d-4cdb-84ef-be81957c9565"


def services(db):
    """Services as `flotilla_core::services` keeps them (kv `services:<thread>`, on the thread's
    VM): the median thread's three, the OpenCode thread's file that doesn't check out. The slug
    thread is up with none, and the uname one stopped (Preview wakes it)."""
    import json
    sandbox = lambda t: db.execute("SELECT sandbox_id FROM threads WHERE id=?", (t,)).fetchone()[0]
    run = lambda **kw: {"name": "", "port": 0, "pid": None, "state": "ready", "exit_code": None, "error": None, "output": "", "preview": None, "links": [],
                        "started_at": "2026-11-01T12:00:00Z", "out_at": 0, "err_at": 0, **kw}
    web = "https://web-4100.preview.example.com"
    vite = "\n".join(["> web@0.1.0 dev", "> vite --port 4100 --host", "", "  VITE v6.2.1  ready in 412 ms", "",
                      "  ➜  Local:   http://localhost:4100/", "  ➜  Network: http://10.0.0.12:4100/", "", "12:00:41 [vite] page reload src/App.tsx"])
    crash = "\n".join(["> api@0.1.0 start", "> node server.js", "", "listening on :4102", "GET /health 200 3ms", "GET /orders 500 41ms",
                       "TypeError: Cannot read properties of undefined (reading 'id')", "    at listOrders (/home/user/app/api/orders.js:18:27)",
                       "    at Layer.handle (/home/user/app/node_modules/express/lib/router/layer.js:95:5)", "    at next (/home/user/app/node_modules/express/lib/router/route.js:149:13)",
                       "Node.js v22.11.0"])
    mine = {"sandbox": sandbox(MEDIAN), "file": ".agents/services.yml", "error": None, "services": [
        run(name="web", port=4100, pid=812, output=vite, preview=f"{web}/?_token=dev", out_at=310,
            links=[{"title": "web", "url": f"{web}/?_token=dev"}, {"title": "Storybook", "url": f"{web}/storybook/?_token=dev"}]),
        run(name="worker", port=4101, state="stopped", started_at=None),
        run(name="api", port=4102, state="exited", exit_code=1, output=crash, preview="https://api-4102.preview.example.com/?_token=dev", out_at=120, err_at=380,
            links=[{"title": "api", "url": "https://api-4102.preview.example.com/?_token=dev"}]),
    ]}
    broken = {"sandbox": sandbox(CHECK), "file": ".agents/services.yml", "error": ".agents/services.yml: service web: no command", "services": []}
    for t, s in ((MEDIAN, mine), (CHECK, broken)):
        db.execute("INSERT OR REPLACE INTO kv (key, value, updated_at) VALUES (?, ?, '2026-11-01T12:00:45.000Z')", (f"services:{t}", json.dumps(s)))
    db.execute("UPDATE threads SET state='idle', keep_running=0 WHERE id IN (?, ?, ?)", (MEDIAN, CHECK, "31360c1e-cbab-4b44-993c-7dec50d661f5"))
    db.execute("UPDATE threads SET state='stopped', last_stopped_at='2026-11-01T11:00:00.000Z', archive_after=NULL WHERE id=?", (UNAME,))
    db.commit()
    print("gave the median thread services, the OpenCode thread a services file with a problem, and stopped the uname thread's VM")


def public(db):
    """Public links as `flotilla_core::public` keeps them (kv `public_link:<thread>:<port>`): the
    median thread's `web` for the next hour, and its port 5173, which no service runs on, until
    stopped."""
    import json
    import time
    tid, sb = MEDIAN, db.execute("SELECT sandbox_id FROM threads WHERE id=?", (MEDIAN,)).fetchone()[0]
    now = int(time.time() * 1000)
    for port, url, until in ((4100, "https://web-4100.preview.example.com", now + 3600 * 1000), (5173, "https://web-5173.preview.example.com", None)):
        link = {"thread": tid, "port": port, "sandbox": sb, "url": url, "since_ms": now - 60_000, "until_ms": until, "ended": False}
        db.execute("INSERT OR REPLACE INTO kv (key, value, updated_at) VALUES (?, ?, '2026-11-01T12:00:45.000Z')", (f"public_link:{tid}:{port}", json.dumps(link)))
    db.commit()
    print("made the median thread's web public for an hour and its port 5173 until stopped")


ASK = "a5a5a5a5-0000-4000-8000-0000000a5c00"


def ask(db, answered):
    """A Codex thread waiting on your answer: a live turn (2026-11-01) whose agent asked which file
    name to create with request_user_input_async. boat showed the question only as text in the
    reply; the question itself is kept beside the thread (`asks:<thread>`), as the server keeps
    what it reads in the VM. Messages are rebuilt from both when the server opens it."""
    import json
    conv, task = "c_ask", "task-ask-1"
    db.execute(
        "INSERT OR REPLACE INTO threads (id, environment_id, environment_name, title, sandbox_id, conversation_id, provider, model, effort, fast, state, keep_running, created_at, updated_at) "
        "VALUES (?, '585998ba-4b24-47db-ac65-904eceb8cffa', 'base', 'Pick a file name', 'bx_ask', ?, 'codex', 'gpt-6-luna', 'medium', 0, 'idle', 0, '2026-11-01T22:12:16.000Z', '2026-11-01T22:12:26.000Z')",
        (ASK, conv),
    )
    for table in ("events", "turns", "messages"):
        db.execute(f"DELETE FROM {table} WHERE thread_id=?", (ASK,))
    shown = "Which file name should I create?\n- alpha.txt (recommended)\n- beta.txt"
    events = [
        ("prompt", task, "ev-ask-prompt", 1793571137797, {"prompt": "Before doing anything else, ask me which file name I want: alpha.txt (recommended) or beta.txt. Then create that file in /home/user containing the word DONE.", "status": "finished", "is_reverted": False}),
        ("response", "ev-ask-reply", "ev-ask-reply", 1793571145446, {"content": shown + "\nWhich file name would you like: **alpha.txt** (recommended) or **beta.txt**?", "model": "gpt-6-luna", "is_reverted": False}),
    ]
    for kind, key, eid, ts, data in events:
        raw = {"conversationId": conv, "data": data, "id": eid, "taskId": task, "timestamp": ts, "type": kind}
        db.execute("INSERT INTO events (thread_id, key, event_id, task_id, type, timestamp, raw_json, settled, received_at) VALUES (?,?,?,?,?,?,?,1,'2026-11-01T22:12:27.000Z')",
                   (ASK, key, eid, task, kind, ts, json.dumps(raw)))
    db.execute("INSERT INTO turns (thread_id, prompt_id, status, done, settled, done_at, created_at) VALUES (?, ?, 'finished', 1, 1, '2026-11-01T22:12:26.000Z', '2026-11-01T22:12:16.000Z')", (ASK, task))
    if answered:
        # The answer as it went (`ask::reply_text`), and the agent's turn on it, as boat showed them.
        nxt = "task-ask-2"
        result = {"content": json.dumps({"type": "bash_output", "pending": False, "command": "printf DONE > /home/user/beta.txt", "description": "", "content": "(no output, exit 0)", "error": False}), "is_error": False, "tool_use_id": "exec-bddd06e3", "type": "tool_result"}
        more = [
            ("prompt", nxt, "ev-ask-answer", 1793571422000, {"prompt": "Which file name should I create?\nbeta.txt", "status": "finished", "is_reverted": False}),
            ("response", "ev-ask-tool", "ev-ask-tool", 1793571425000, {"content": "", "model": "gpt-6-luna", "is_reverted": False, "tools": [{"result": result, "use": {"id": "exec-bddd06e3", "input": {"command": "printf DONE > /home/user/beta.txt"}, "name": "Bash", "type": "tool_use"}}]}),
            ("response", "ev-ask-done", "ev-ask-done", 1793571425500, {"content": "Created `beta.txt` in `/home/user` containing `DONE`.", "model": "gpt-6-luna", "is_reverted": False}),
        ]
        for kind, key, eid, ts, data in more:
            raw = {"conversationId": conv, "data": data, "id": eid, "taskId": nxt, "timestamp": ts, "type": kind}
            db.execute("INSERT INTO events (thread_id, key, event_id, task_id, type, timestamp, raw_json, settled, received_at) VALUES (?,?,?,?,?,?,?,1,'2026-11-01T22:17:06.000Z')",
                       (ASK, key, eid, nxt, kind, ts, json.dumps(raw)))
        db.execute("INSERT INTO turns (thread_id, prompt_id, status, done, settled, done_at, created_at) VALUES (?, ?, 'finished', 1, 1, '2026-11-01T22:17:06.000Z', '2026-11-01T22:17:02.000Z')", (ASK, nxt))
        db.execute("INSERT OR REPLACE INTO sent_prompts (thread_id, id, kind, text, sent_at) VALUES (?, ?, 'prompt', ?, '2026-11-01T22:17:02.000Z')", (ASK, nxt, "Which file name should I create?\nbeta.txt"))
    asked = [{"id": "call_4e640a800d854e80a3eff9c38bdbf4bc", "at_ms": 1793571145443, "text": shown, "tool": "request_user_input_async",
              "questions": [{"question": "Which file name should I create?", "options": [{"label": "alpha.txt (recommended)"}, {"label": "beta.txt"}]}]}]
    db.execute("INSERT OR REPLACE INTO kv (key, value, updated_at) VALUES (?, ?, '2026-11-01T22:12:27.000Z')", (f"asks:{ASK}", json.dumps(asked)))
    db.execute("DELETE FROM kv WHERE key=?", (f"answers:{ASK}",))
    if answered:
        db.execute("INSERT OR REPLACE INTO kv (key, value, updated_at) VALUES (?, ?, '2026-11-01T22:17:02.000Z')",
                   (f"answers:{ASK}", json.dumps({"call_4e640a800d854e80a3eff9c38bdbf4bc": {"answers": [["beta.txt"]], "local": nxt}})))
    db.execute("INSERT OR REPLACE INTO thread_views (thread_id, viewed_at) VALUES (?, '2026-01-01T00:00:00Z')", (ASK,))
    db.execute("DELETE FROM kv WHERE key='messages_version'")
    db.commit()
    print(f"added the thread {ASK} with a question {'answered' if answered else 'waiting'}")


def stress(db, turns):
    """A long thread from boat-shaped events: each turn a prompt, a dozen tool calls and a reply;
    every tenth a huge Bash output, a 2,000-line Read and a big Write. Messages are rebuilt from
    the events when the server opens the database."""
    import json
    conv = "c_stress"
    db.execute(
        "INSERT OR REPLACE INTO threads (id, environment_id, environment_name, title, sandbox_id, conversation_id, provider, model, effort, fast, state, keep_running, created_at, updated_at) "
        "VALUES (?, '585998ba-4b24-47db-ac65-904eceb8cffa', 'base', 'Stress: a long thread', 'bx_stress', ?, 'claude-code', 'claude-sonnet-5', NULL, 0, 'stopped', 0, '2026-10-30T09:00:00.000Z', '2026-10-31T13:00:00.000Z')",
        (STRESS, conv),
    )
    for table in ("events", "turns", "messages"):
        db.execute(f"DELETE FROM {table} WHERE thread_id=?", (STRESS,))
    ts = 1793300000000
    rows = []
    n_tools = 0

    def event(kind, eid, task, data):
        nonlocal ts
        ts += 1500
        raw = {"conversationId": conv, "data": data, "id": eid, "taskId": task, "timestamp": ts, "type": kind}
        key = task if kind == "prompt" else eid
        rows.append((STRESS, key, eid, task, kind, ts, json.dumps(raw), 1, "2026-10-31T13:00:00.000Z"))

    def tool(task, name, inp, result):
        nonlocal n_tools
        n_tools += 1
        tid = f"toolu_stress{n_tools:06d}"
        res = {"content": json.dumps(result), "is_error": bool(result.get("error")), "tool_use_id": tid, "type": "tool_result"}
        event("response", f"ev-tool-{n_tools}", task, {"content": "", "is_reverted": False, "model": "claude-sonnet-5",
              "tools": [{"result": res, "use": {"id": tid, "input": inp, "name": name, "type": "tool_use"}}]})

    words = "the quick brown fox jumps over the lazy dog while the retry loop backs off and the flaky test settles".split()
    for t in range(turns):
        task = f"task-stress-{t:04d}"
        event("prompt", f"ev-prompt-{t}", task, {"is_reverted": False, "status": "finished",
              "prompt": f"Turn {t + 1}: look at the retry module again, run the tests, and tell me what changed. " + " ".join(words[: 8 + t % 9])})
        db.execute("INSERT OR REPLACE INTO turns (thread_id, prompt_id, status, done, settled, done_at, created_at) VALUES (?, ?, 'finished', 1, 1, '2026-10-31T13:00:00.000Z', '2026-10-31T13:00:00.000Z')", (STRESS, task))
        for k in range(12):
            path = f"/home/user/slug/src/retry_{(t * 12 + k) % 37}.py"
            if k % 4 == 0:
                out = "\n".join(f"test_retry_{i} PASSED" for i in range(8 + k))
                tool(task, "Bash", {"command": f"pytest -q tests/test_retry_{k}.py", "description": "Run the retry tests"},
                     {"type": "bash_output", "pending": False, "command": f"pytest -q tests/test_retry_{k}.py", "content": out, "error": False})
            elif k % 4 == 1:
                tool(task, "Read", {"file_path": path}, {"type": "file_content", "pending": False, "filePath": path, "fileName": path.rsplit("/", 1)[1],
                     "language": "python", "totalLines": 40, "lines": [{"number": i} for i in range(1, 41)]})
            elif k % 4 == 2:
                tool(task, "Edit", {"file_path": path, "old_string": "delay = 1", "new_string": "delay = min(2 ** attempt, 30)", "replace_all": False},
                     {"type": "file_edit", "pending": False, "filePath": path, "fileName": path.rsplit("/", 1)[1], "isMultiEdit": False, "editsCount": 1,
                      "oldString": "delay = 1", "newString": "delay = min(2 ** attempt, 30)"})
            else:
                tool(task, "Grep", {"pattern": "backoff", "path": "/home/user/slug"},
                     {"type": "generic_tool", "pending": False, "toolName": "Grep", "output": "\n".join(f"src/retry_{i}.py:{i * 3}: backoff" for i in range(12))})
        if t % 10 == 5:
            # Huge: a 400 KB log, a 2,000-line read, a 120 KB file written with multibyte text
            # across the 32 KB cut.
            log = "\n".join(f"[{i:06d}] retry attempt {i % 7} for job {i} backed off {i % 30}s — ok ✓" for i in range(5200))
            tool(task, "Bash", {"command": "cat logs/retry.log", "description": "Read the retry log"},
                 {"type": "bash_output", "pending": False, "command": "cat logs/retry.log", "content": log, "error": False})
            tool(task, "Read", {"file_path": "/home/user/slug/src/big.py"}, {"type": "file_content", "pending": False, "filePath": "/home/user/slug/src/big.py",
                 "fileName": "big.py", "language": "python", "totalLines": 2000, "lines": [{"number": i, "content": f"    value_{i} = compute({i})  # 日本語のコメント {i}"} for i in range(1, 2001)]})
            body = ("é" * 16383) + "日本語" + ("ü✓" * 30000)
            tool(task, "Write", {"file_path": "/home/user/slug/notes.md", "content": body},
                 {"type": "file_write", "pending": False, "filePath": "/home/user/slug/notes.md", "fileName": "notes.md", "content": body})
        event("response", f"ev-text-{t}", task, {"content": f"Turn {t + 1} done. The retry loop now backs off exponentially, capped at 30 s; "
              + " ".join(words) + f" — {t + 1} of {turns}.", "is_reverted": False, "model": "claude-sonnet-5"})
    db.executemany("INSERT OR REPLACE INTO events (thread_id, key, event_id, task_id, type, timestamp, raw_json, settled, received_at) VALUES (?,?,?,?,?,?,?,?,?)", rows)
    # Messages are derived: the server rebuilds them all when it opens the database.
    db.execute("DELETE FROM kv WHERE key='messages_version'")
    db.commit()
    print(f"added the stress thread {STRESS}: {turns} turns, {len(rows)} events")


if __name__ == "__main__":
    main()
