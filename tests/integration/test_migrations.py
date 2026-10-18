import sqlite3
import unittest
from pathlib import Path

MIGRATION = Path(__file__).resolve().parents[2] / 'migrations/0001_init.sql'


class StorageInvariants(unittest.TestCase):
    def setUp(self):
        self.db = sqlite3.connect(':memory:')
        self.db.execute('PRAGMA foreign_keys = ON')
        self.db.executescript(MIGRATION.read_text())
        for meeting in ('m1', 'm2'):
            self.db.execute("INSERT INTO meetings(id,title,timezone,created_at) VALUES (?,?,?,?)",
                            (meeting, '测试会议', 'Asia/Shanghai', '2026-10-18T05:00:00Z'))

    def tearDown(self):
        self.db.close()

    def add_segment(self, segment, meeting):
        self.db.execute("INSERT INTO transcript_segments VALUES (?,?,0,1000,'mixed',1,'final')", (segment, meeting))
        self.db.execute("INSERT INTO transcript_revisions VALUES (?,1,?,'原文','asr','2026-10-18T05:00:01Z')", (segment, meeting))

    def test_migration_integrity(self):
        self.assertEqual(self.db.execute('PRAGMA user_version').fetchone()[0], 1)
        self.assertEqual(self.db.execute('PRAGMA integrity_check').fetchone()[0], 'ok')
        self.assertEqual(self.db.execute('PRAGMA foreign_key_check').fetchall(), [])

    def test_only_one_active_meeting(self):
        self.db.execute("UPDATE meetings SET state='paused' WHERE id='m1'")
        with self.assertRaises(sqlite3.IntegrityError):
            self.db.execute("UPDATE meetings SET state='recording' WHERE id='m2'")

    def test_audio_time_ranges_must_be_valid(self):
        with self.assertRaises(sqlite3.IntegrityError):
            self.db.execute("INSERT INTO audio_chunks VALUES ('a','m1','microphone','audio/a.wav',100,50,0,'sealed')")

    def test_cross_meeting_and_missing_revision_evidence_rejected(self):
        self.add_segment('s1', 'm1')
        self.add_segment('s2', 'm2')
        self.db.execute("INSERT INTO artifacts VALUES ('a1','m1','stage',1,'{}','{}','draft','model')")
        for segment, revision in [('s2', 1), ('s1', 2)]:
            with self.assertRaises(sqlite3.IntegrityError):
                self.db.execute("INSERT INTO artifact_evidence VALUES ('a1','decision','m1',?,?)", (segment, revision))
        self.db.execute("INSERT INTO artifact_evidence VALUES ('a1','decision','m1','s1',1)")

    def test_job_retries_cannot_duplicate_idempotency_key(self):
        args = ('job1', 'm1', 'same-key', 'stage', '{}', 1, 'pending', 0, None)
        self.db.execute('INSERT INTO analysis_jobs VALUES (?,?,?,?,?,?,?,?,?)', args)
        with self.assertRaises(sqlite3.IntegrityError):
            self.db.execute('INSERT INTO analysis_jobs VALUES (?,?,?,?,?,?,?,?,?)', ('job2', *args[1:]))

    def test_human_revision_preserves_original_evidence(self):
        self.add_segment('s1', 'm1')
        self.db.execute("INSERT INTO transcript_revisions VALUES ('s1',2,'m1','修订','human','2026-10-18T05:00:02Z')")
        self.db.execute("UPDATE transcript_segments SET current_revision=2, state='edited' WHERE id='s1'")
        rows = self.db.execute("SELECT text FROM transcript_revisions WHERE segment_id='s1' ORDER BY revision").fetchall()
        self.assertEqual(rows, [('原文',), ('修订',)])

    def test_cascade_deletes_relational_data(self):
        self.add_segment('s1', 'm1')
        self.db.execute("INSERT INTO artifacts VALUES ('a1','m1','stage',1,'{}','{}','draft','model')")
        self.db.execute("INSERT INTO artifact_evidence VALUES ('a1','x','m1','s1',1)")
        self.db.execute("INSERT INTO cleanup_queue VALUES ('c1','m1','audio/a.wav',0,NULL)")
        self.db.execute("DELETE FROM meetings WHERE id='m1'")
        for table in ('transcript_segments', 'transcript_revisions', 'artifacts', 'artifact_evidence', 'cleanup_queue'):
            self.assertEqual(self.db.execute(f'SELECT COUNT(*) FROM {table}').fetchone()[0], 0)
        self.assertEqual(self.db.execute('PRAGMA foreign_key_check').fetchall(), [])


class ModelProfileMigrations(unittest.TestCase):
    def setUp(self):
        self.db = sqlite3.connect(':memory:')
        self.db.execute('PRAGMA foreign_keys=ON')
        for migration in sorted(MIGRATION.parent.glob('*.sql')):
            self.db.executescript(migration.read_text())
        self.db.execute("INSERT INTO meetings(id,title,timezone,created_at) VALUES ('m','测试','Asia/Shanghai','2026-10-18T00:00:00Z')")
        self.db.execute("INSERT INTO model_configs VALUES ('p',1,'{}',NULL)")
        self.db.execute("INSERT INTO model_profiles VALUES ('p',1,0)")

    def tearDown(self):
        self.db.close()

    def test_latest_schema_and_immutable_model_versions(self):
        self.assertEqual(self.db.execute('PRAGMA user_version').fetchone()[0], 6)
        for statement in ("UPDATE model_configs SET config_json='{}'", 'DELETE FROM model_configs'):
            with self.assertRaises(sqlite3.IntegrityError):
                self.db.execute(statement)
        self.assertEqual(self.db.execute('PRAGMA foreign_key_check').fetchall(), [])

    def test_binding_requires_real_version_and_survives_profile_archive(self):
        with self.assertRaises(sqlite3.IntegrityError):
            self.db.execute("INSERT INTO meeting_model_bindings VALUES ('m','p',2)")
        self.db.execute("INSERT INTO meeting_model_bindings VALUES ('m','p',1)")
        self.db.execute("UPDATE model_profiles SET archived=1 WHERE id='p'")
        self.assertEqual(self.db.execute('SELECT config_version FROM meeting_model_bindings').fetchone()[0], 1)

    def test_source_states_and_meeting_deletion_preserve_model_history(self):
        self.db.execute("INSERT INTO recording_sources(meeting_id,source,state) VALUES ('m','microphone','disabled')")
        with self.assertRaises(sqlite3.IntegrityError):
            self.db.execute("UPDATE recording_sources SET state='normal'")
        self.db.execute("INSERT INTO meeting_model_bindings VALUES ('m','p',1)")
        self.db.execute("DELETE FROM meetings WHERE id='m'")
        self.assertEqual(self.db.execute('SELECT count(*) FROM recording_sources').fetchone()[0], 0)
        self.assertEqual(self.db.execute('SELECT count(*) FROM meeting_model_bindings').fetchone()[0], 0)
        self.assertEqual(self.db.execute('SELECT count(*) FROM model_configs').fetchone()[0], 1)


if __name__ == '__main__':
    unittest.main()
