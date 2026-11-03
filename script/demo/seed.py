#!/usr/bin/env python3
"""Make the demo server's threads (crates/flotilla-engine/src/demo/seed.sql) from a recording.

    python3 -I script/demo/seed.py <recording.db> <flotilla binary>

The recording is the video's database (/tmp/flotilla-ux3/dbs/handoff.db), copied, and opened once
by `flotilla` to bring it to this build's schema. Then:
- dev-db.py's scrub: VM hostnames, tailnet names and the names in ~/.config/flotilla-dev/scrub;
- the GitHub owner (read from its pull requests' links) reads as `acme`, and tokens go;
- every VM is stopped, nothing has expired or is running, and #19 is open, approved and mergeable;
- its project is there, as `acme/<repo>`;
- of what's kept beside the threads, only what their views read: the models, their skills and
  files, hand-offs and agents.

It's written as INSERTs (messages are rebuilt from the events as it loads), and nothing is written
if any of it still names a real machine, an account or a token.
"""
import importlib.util
import os
import re
import sqlite3
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
OUT = os.path.join(ROOT, "crates/flotilla-engine/src/demo/seed.sql")
TABLES = ["environments", "projects", "project_scripts", "setup_runs", "threads", "events", "turns",
          "sent_prompts", "thread_views", "thread_archive", "summaries", "thread_prs", "kv"]
TOKEN = re.compile(r"\b(gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|sk-ant-[A-Za-z0-9_-]{10,}|xox[abp]-[A-Za-z0-9-]{10,})@?")
EMAIL = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}")
CHECKS = '[{"name":"build","state":"pass","url":null},{"name":"test","state":"pass","url":null}]'


def dev_db():
    spec = importlib.util.spec_from_file_location("dev_db", os.path.join(ROOT, "ios/script/dev-db.py"))
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


def text_columns(db, table):
    return [r[1] for r in db.execute(f'PRAGMA table_info("{table}")') if r[2].upper() in ("TEXT", "")]


def main():
    source, flotilla = sys.argv[1], sys.argv[2]
    work = tempfile.mkdtemp(prefix="flotilla-seed-")
    path = os.path.join(work, "flotilla.db")
    src = sqlite3.connect(f"file:{source}?mode=ro", uri=True)
    db = sqlite3.connect(path)
    src.backup(db)
    src.close()
    db.close()
    # This build's schema.
    subprocess.run([flotilla, "--db", path, "devices"], check=True, capture_output=True)
    db = sqlite3.connect(path)
    dev_db().scrub(db)

    owners = {m.group(1) for (url,) in db.execute("SELECT url FROM thread_prs") for m in [re.search(r"github\.com/([^/]+)/", url)] if m}
    repos = {m.group(1) for (url,) in db.execute("SELECT url FROM thread_prs") for m in [re.search(r"github\.com/[^/]+/([^/]+)/", url)] if m}
    db.create_function("tidy", 1, lambda v: v if not isinstance(v, str) else tidy(v, owners), deterministic=True)
    for table in TABLES:
        for c in text_columns(db, table):
            db.execute(f'UPDATE "{table}" SET "{c}" = tidy("{c}")')

    # Only what the threads' views read.
    ids = [r[0] for r in db.execute("SELECT id FROM threads")]
    keep = ["catalog"] + [f"{p}%{t}%" for t in ids for p in ("mentions:", "lineage:", "orch:", "side:")]
    db.execute("DELETE FROM kv WHERE NOT (" + " OR ".join("key LIKE ?" for _ in keep) + ")", keep)
    for table in ("events", "turns", "sent_prompts", "thread_views", "thread_archive", "summaries", "thread_prs"):
        db.execute(f"DELETE FROM {table} WHERE thread_id NOT IN ({','.join('?' * len(ids))})", ids)

    # Stopped VMs, nothing expired or running, and files sent with nothing left to open.
    db.execute("UPDATE threads SET state='stopped', data_expired_at=NULL, keep_running=0, archive_after=NULL")
    db.execute("UPDATE turns SET done=1, settled=1")
    db.execute("UPDATE events SET settled=1")
    db.execute("UPDATE sent_prompts SET attachments=json_replace(attachments, '$[0].blob', '') WHERE attachments LIKE '%\"blob\"%'")
    # #19, still to merge; its project, as the threads have it.
    db.execute("UPDATE thread_prs SET state='OPEN', mergeable='MERGEABLE', review='APPROVED', checks_json=?, additions=38, deletions=4 WHERE number=19", (CHECKS,))
    env = db.execute("SELECT name FROM environments WHERE is_default=0 ORDER BY name LIMIT 1").fetchone()
    for (project,) in db.execute("SELECT DISTINCT repo_project_id FROM threads WHERE repo_project_id IS NOT NULL").fetchall():
        db.execute(
            "INSERT OR IGNORE INTO projects (id, owner, repo, default_branch, environment_name, created_at, template_sha, template_built_at) "
            "VALUES (?, 'acme', ?, 'main', ?, '2026-10-31T12:00:00.000Z', 'c5842f16d09d671879efa25b369524dde904ba31', '2026-10-31T12:00:00.000Z')",
            (project, sorted(repos)[0], env[0] if env else "base"),
        )
    db.commit()

    # Nothing written if anything's left.
    left = dev_db().leaks(db)
    words = [w for o in owners for w in [o] + re.split(r"[-_.]", o) if len(w) >= 4]
    for table in TABLES:
        for c in text_columns(db, table):
            for (v,) in db.execute(f'SELECT "{c}" FROM "{table}" WHERE "{c}" IS NOT NULL'):
                why = leak(v, words)
                if why:
                    left.append((table, c, why))
    if left:
        sys.exit("still there: " + ", ".join(f"{t}.{c} ({n})" for t, c, n in left))

    with open(OUT, "w") as f:
        f.write("-- The demo server's threads (`flotilla serve --demo`), made by script/demo/seed.py from a\n")
        f.write("-- recording: make it again rather than editing it.\n")
        for table in TABLES:
            cols = [r[1] for r in db.execute(f'PRAGMA table_info("{table}")')]
            for row in db.execute(f'SELECT {", ".join(cols)} FROM "{table}" ORDER BY rowid'):
                f.write(f"INSERT INTO {table} ({', '.join(cols)}) VALUES ({', '.join(literal(v) for v in row)});\n")
    print(f"{len(ids)} threads, {os.path.getsize(OUT) // 1024} KB: {OUT}")


def tidy(v, owners):
    v = TOKEN.sub("", v)
    for o in owners:
        v = re.sub(re.escape(o), "acme", v, flags=re.I)
    return v


def leak(v, words):
    if not isinstance(v, str):
        return None
    low = v.lower()
    for w in words:
        if w.lower() in low:
            return "the owner's name"
    for pattern, why in ((TOKEN, "a token"), (EMAIL, "an email address")):
        if pattern.search(v):
            return why
    # Machine names that must not appear come from ~/.config/flotilla-dev/scrub or
    # $DEV_DB_SCRUB (dev-db.py's list), so they stay out of the repo.
    for w in ("/users/", "localhost:", *(n.lower() for n in private_names())):
        if w in low:
            return w
    return None


def private_names():
    path = os.path.expanduser("~/.config/flotilla-dev/scrub")
    listed = open(path).read().splitlines() if os.path.exists(path) else []
    listed += os.environ.get("DEV_DB_SCRUB", "").split(",")
    return [n.strip() for n in listed if n.strip()]


def literal(v):
    if v is None:
        return "NULL"
    if isinstance(v, (int, float)):
        return repr(v)
    if isinstance(v, bytes):
        return f"X'{v.hex()}'"
    return "'" + v.replace("'", "''") + "'"


if __name__ == "__main__":
    main()
