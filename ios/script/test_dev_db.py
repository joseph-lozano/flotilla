#!/usr/bin/env python3
"""Checks for dev-db.py's scrub: `python3 -I ios/script/test_dev_db.py`."""
import importlib.util
import os
import sqlite3
import unittest

spec = importlib.util.spec_from_file_location("dev_db", os.path.join(os.path.dirname(os.path.abspath(__file__)), "dev-db.py"))
dev_db = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dev_db)


def made():
    db = sqlite3.connect(":memory:")
    db.executescript(
        """
        CREATE TABLE events (thread_id TEXT, data TEXT);
        CREATE TABLE kv (key TEXT, value TEXT);
        CREATE TABLE devices (id TEXT, name TEXT, last_ip TEXT);
        CREATE TABLE pairing_codes (id TEXT, made_by TEXT);
        CREATE TABLE schedules (id TEXT, prompt TEXT);
        """
    )
    db.executemany("INSERT INTO events VALUES ('t', ?)", [("ssh box.my-net.ts.net",), ("on box-node-abcdef12 at Box.Office-Net.ts.net:443",), ("https://box.tail1234.ts.net",)])
    db.execute("INSERT INTO devices VALUES ('d1', 'Work laptop', '100.101.102.103')")
    db.execute("INSERT INTO pairing_codes VALUES ('c1', 'cli')")
    db.commit()
    return db


class Scrub(unittest.TestCase):
    def test_any_tailnet_name_reads_as_the_stand_in(self):
        db = made()
        dev_db.scrub(db)
        texts = [r[0] for r in db.execute("SELECT data FROM events ORDER BY rowid")]
        self.assertEqual(texts, ["ssh box.tail1234.ts.net", "on box at Box.tail1234.ts.net:443", "https://box.tail1234.ts.net"])

    def test_paired_devices_and_codes_dont_come_along(self):
        db = made()
        dev_db.scrub(db)
        self.assertEqual(db.execute("SELECT count(*) FROM devices").fetchone()[0], 0)
        self.assertEqual(db.execute("SELECT count(*) FROM pairing_codes").fetchone()[0], 0)

    def test_a_name_left_anywhere_is_found(self):
        db = made()
        dev_db.scrub(db)
        self.assertEqual(dev_db.leaks(db), [])
        db.execute("INSERT INTO schedules VALUES ('s1', 'deploy to ci.corp-net.ts.net')")
        self.assertEqual(dev_db.leaks(db), [("schedules", "prompt", 1)])


if __name__ == "__main__":
    unittest.main()
