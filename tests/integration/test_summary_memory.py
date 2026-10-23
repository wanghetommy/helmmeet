import sqlite3
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


class SummaryMemoryUpgrade(unittest.TestCase):
    def setUp(self):
        self.db = sqlite3.connect(':memory:')
        self.db.execute('PRAGMA foreign_keys=ON')
        for migration in sorted((ROOT/'migrations').glob('*.sql')):
            if migration.name.startswith('0012_'):
                continue
            self.db.executescript(migration.read_text())
        for name, mode, enabled in [('old-minute', 'minute', 0), ('old-live', 'realtime', 1)]:
            self.db.execute("INSERT INTO meetings(id,title,timezone,created_at) VALUES (?,?,'Asia/Shanghai','now')", (name,name))
            self.db.execute('INSERT INTO summary_schedules(meeting_id,mode,enabled) VALUES (?,?,?)', (name,mode,enabled))
        self.db.commit()
        self.db.executescript((ROOT/'migrations/0012_summary_memory.sql').read_text())

    def tearDown(self):
        self.db.close()

    def test_upgrade_preserves_consent_and_maps_only_removed_mode(self):
        self.assertEqual(self.db.execute('PRAGMA user_version').fetchone()[0], 12)
        self.assertEqual(self.db.execute("SELECT mode,enabled,version FROM summary_schedules WHERE meeting_id='old-minute'").fetchone(), ('three_minutes',0,2))
        self.assertEqual(self.db.execute("SELECT mode,enabled FROM summary_schedules WHERE meeting_id='old-live'").fetchone(), ('realtime',1))
        self.assertTrue(self.db.execute("SELECT migration_notice FROM summary_schedules WHERE meeting_id='old-minute'").fetchone()[0])
        self.db.execute("UPDATE summary_schedules SET mode='economy' WHERE meeting_id='old-live'")
        with self.assertRaises(sqlite3.IntegrityError):
            self.db.execute("UPDATE summary_schedules SET mode='minute' WHERE meeting_id='old-live'")
        self.assertEqual(self.db.execute('PRAGMA foreign_key_check').fetchall(), [])

    def test_fragment_and_memory_indexes_cascade_with_source(self):
        db=self.db
        db.execute("INSERT INTO transcript_segments VALUES ('s','old-live',0,1000,'mixed',1,'final')")
        db.execute("INSERT INTO transcript_revisions VALUES ('s',1,'old-live','合成文字','asr','now')")
        db.execute("INSERT INTO artifacts VALUES ('a','old-live','stage',1,'{}','{}','draft','model')")
        db.execute("INSERT INTO meeting_memories VALUES ('a','old-live',0,0,'[]','now')")
        db.execute("INSERT INTO summary_fragments VALUES ('old-live','s',1,0,4,'a')")
        with self.assertRaises(sqlite3.IntegrityError):
            db.execute("INSERT INTO summary_fragments VALUES ('old-minute','s',1,0,4,'a')")
        db.execute("DELETE FROM meetings WHERE id='old-live'")
        self.assertEqual(db.execute('SELECT count(*) FROM meeting_memories').fetchone()[0],0)
        self.assertEqual(db.execute('SELECT count(*) FROM summary_fragments').fetchone()[0],0)
