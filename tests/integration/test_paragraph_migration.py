import sqlite3
import unittest
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
class ParagraphMigration(unittest.TestCase):
    def test_upgrade_preserves_sources_and_isolates_epochs_and_cascades(self):
        db=sqlite3.connect(':memory:');db.execute('PRAGMA foreign_keys=ON')
        for path in sorted((ROOT/'migrations').glob('*.sql')):db.executescript(path.read_text())
        for mid in ['a','b']:
            db.execute("INSERT INTO meetings(id,title,timezone,created_at) VALUES (?,?,'Asia/Shanghai','synthetic')",(mid,mid))
        db.execute("INSERT INTO recording_epochs(id,meeting_id,recording_start_ms,wall_start,reason) VALUES ('e','a',0,'synthetic','recording')")
        with self.assertRaises(sqlite3.IntegrityError):db.execute("INSERT INTO speech_activity_epochs VALUES ('e','b')")
        db.execute("INSERT INTO speech_activity_epochs VALUES ('e','a')")
        db.execute("INSERT INTO speech_pauses VALUES ('a','e',1500,4500)")
        with self.assertRaises(sqlite3.IntegrityError):db.execute("INSERT INTO speech_pauses VALUES ('b','e',1500,4500)")
        db.execute("DELETE FROM meetings WHERE id='a'")
        self.assertEqual(db.execute('SELECT count(*) FROM speech_pauses').fetchone()[0],0)
        self.assertEqual(db.execute('PRAGMA foreign_key_check').fetchall(),[])
        self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0],18)
        db.close()
