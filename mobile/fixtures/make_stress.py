"""Make the `stress` fixtures from the real `side-live4` dump: a sidebar of 40 rows and a thread
with its messages repeated to about 500, for scrolling, plus an open, mergeable pull request on
the first row so the thread page shows its PR bar and Hold to merge. Everything but the repeats,
the titles and the PR is the real data.

    python3 -I make_stress.py side-live4 stress
"""

import copy
import json
import sys
from pathlib import Path

src, dst = Path(sys.argv[1]), Path(sys.argv[2])
dst.mkdir(exist_ok=True)
sidebar = json.loads((src / "sidebar.json").read_text())
catalog = (src / "catalog.json").read_text()
real = sidebar["threads"][0]
tid = real["thread"]["id"]
thread = json.loads((src / f"thread-{tid}.json").read_text())

TITLES = [
    "Fix flaky retry test", "Review median.py", "Add OAuth device flow", "Slugify edge cases",
    "Sidebar remembers groups", "Paged thread history", "Cap tool output on the wire",
    "Linux build in CI", "Secret Service for device tokens", "Web Push for the PWA",
]
ACTIVITY = ["unread", "unread", "working", "working", "idle"]

rows = []
for i in range(40):
    row = copy.deepcopy(real)
    row["thread"]["id"] = tid if i == 0 else f"{tid[:-4]}{i:04d}"
    row["thread"]["title"] = real["thread"]["title"] if i == 0 else f"{TITLES[i % len(TITLES)]}" + (f" ({i // len(TITLES) + 1})" if i >= len(TITLES) else "")
    row["activity"] = "idle" if i == 0 else ACTIVITY[i % len(ACTIVITY)]
    row["last_activity_ms"] = real["last_activity_ms"] - i * 600_000
    if row["activity"] == "working":
        row["turn_started_ms"] = real["last_activity_ms"] - i * 20_000
    rows.append(row)

# Synthetic: an open PR that can merge, on the real thread.
rows[0]["pr"] = [{
    "number": 42, "url": "https://github.com/example/slug/pull/42", "title": "Fix slugify's trailing hyphen and truncation",
    "head": "flot/fix-slugify", "base": "main", "state": "OPEN", "is_draft": False, "mergeable": "MERGEABLE",
    "review": None, "additions": 48, "deletions": 12,
    "checks": [{"name": f"test ({n})", "state": "pass", "url": None} for n in ("linux", "macos", "lint")],
    "updated_at": None,
}, False]
sidebar["threads"] = rows
(dst / "sidebar.json").write_text(json.dumps(sidebar, indent=1))
(dst / "catalog.json").write_text(catalog)

# The real thread's messages, 60 times over, renumbered.
messages = []
for r in range(60):
    for m in thread["messages"]:
        m = copy.deepcopy(m)
        m["seq"] = len(messages)
        for key in ("event_key", "tool_id", "task_id"):
            if r and m.get(key):
                m[key] = f"{m[key]}-{r}"
        messages.append(m)
long = copy.deepcopy(thread)
long["messages"] = messages
(dst / f"thread-{tid}.json").write_text(json.dumps(long, indent=1))
print(f"{len(rows)} rows, {len(messages)} messages")
