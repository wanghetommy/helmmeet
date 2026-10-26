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
        self.assertEqual(self.db.execute('PRAGMA user_version').fetchone()[0], 24)
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

    def test_summary_manifest_is_bound_to_same_meeting_revision(self):
        self.db.execute("INSERT INTO transcript_segments VALUES ('s','m',0,1000,'mixed',1,'final')")
        self.db.execute("INSERT INTO transcript_revisions VALUES ('s',1,'m','fixture','asr','now')")
        self.db.execute("INSERT INTO summary_coverage VALUES ('m','s',1)")
        with self.assertRaises(sqlite3.IntegrityError):
            self.db.execute("UPDATE summary_coverage SET revision=2")
        self.db.execute("DELETE FROM meetings WHERE id='m'")
        self.assertEqual(self.db.execute('SELECT count(*) FROM summary_coverage').fetchone()[0], 0)

    def test_template_versions_and_stage_coalescence(self):
        self.db.execute("INSERT INTO template_versions VALUES ('weekly',1,'Weekly','fixture')")
        with self.assertRaises(sqlite3.IntegrityError):
            self.db.execute("UPDATE template_versions SET instructions='changed'")
        self.db.execute("INSERT INTO analysis_jobs VALUES ('j','m','key','stage','{}',1,'pending',0,NULL)")
        with self.assertRaises(sqlite3.IntegrityError):
            self.db.execute("INSERT INTO analysis_jobs VALUES ('j2','m','key2','stage','{}',1,'pending',0,NULL)")

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

class AudioEnhancementMigration(unittest.TestCase):
    def test_v7_upgrade_preserves_meetings_off_and_constraints_cascade(self):
        db = sqlite3.connect(':memory:')
        db.execute('PRAGMA foreign_keys=ON')
        for migration in sorted(MIGRATION.parent.glob('*.sql')):
            if migration.name.startswith('0008'):
                db.execute("INSERT INTO meetings(id,title,timezone,created_at) VALUES('old','旧会议','Asia/Shanghai','x')")
                db.commit()
            db.executescript(migration.read_text())
        self.assertEqual(db.execute("SELECT mode FROM audio_enhancement WHERE meeting_id='old'").fetchone()[0], 'off')
        db.execute("UPDATE audio_enhancement SET mode='low' WHERE meeting_id='old'")
        db.execute("INSERT INTO audio_chunks VALUES('raw','old','microphone','a.wav',0,10,160,'sealed')")
        db.execute("INSERT INTO enhanced_chunks SELECT 'raw','old','ns.wav',160,engine FROM audio_enhancement WHERE meeting_id='old'")
        db.execute("UPDATE meetings SET state='recording' WHERE id='old'")
        with self.assertRaises(sqlite3.IntegrityError):
            db.execute("UPDATE audio_enhancement SET mode='moderate' WHERE meeting_id='old'")
        db.execute("INSERT INTO meetings(id,title,timezone,created_at) VALUES('other','另一场','Asia/Shanghai','x')")
        db.execute("INSERT INTO audio_enhancement(meeting_id,mode) VALUES('other','low')")
        with self.assertRaises(sqlite3.IntegrityError):
            db.execute("INSERT INTO enhanced_chunks SELECT 'raw','other','cross.wav',160,engine FROM audio_enhancement WHERE meeting_id='other'")
        db.execute("DELETE FROM meetings WHERE id='old'")
        self.assertEqual(db.execute('SELECT count(*) FROM enhanced_chunks').fetchone()[0], 0)
        self.assertEqual(db.execute('PRAGMA foreign_key_check').fetchall(), [])
        db.close()

class EchoMigration(unittest.TestCase):
    def test_v8_upgrade_retains_ns_freezes_echo_and_rejects_cross_meeting_timing(self):
        db = sqlite3.connect(':memory:')
        db.execute('PRAGMA foreign_keys=ON')
        for migration in sorted(MIGRATION.parent.glob('*.sql')):
            if migration.name.startswith('0009'):
                break
            db.executescript(migration.read_text())
        db.execute("INSERT INTO meetings(id,title,timezone,created_at) VALUES('old','旧会议','Asia/Shanghai','x')")
        db.execute("INSERT INTO audio_enhancement(meeting_id,mode) VALUES('old','low')")
        db.commit()
        db.executescript((MIGRATION.parent / '0009_echo_cancellation.sql').read_text())
        self.assertEqual(db.execute('SELECT mode,echo_cancellation FROM audio_enhancement').fetchone(), ('low', 0))
        self.assertEqual(db.execute('SELECT count(*) FROM audio_chunk_timing').fetchone()[0], 0)
        db.execute("INSERT INTO meetings(id,title,timezone,created_at) VALUES('other','另一场','Asia/Shanghai','x')")
        for epoch, meeting in [('e1', 'old'), ('e2', 'other')]:
            db.execute("INSERT INTO recording_epochs(id,meeting_id,recording_start_ms,wall_start,reason) VALUES(?,?,0,'x','recording')", (epoch, meeting))
        db.execute("INSERT INTO audio_chunks VALUES('raw','old','microphone','raw.wav',0,10,160,'sealed')")
        with self.assertRaises(sqlite3.IntegrityError):
            db.execute("INSERT INTO audio_chunk_timing VALUES('raw','old','e2',5,1000)")
        db.execute("INSERT INTO audio_chunk_timing VALUES('raw','old','e1',5,1000)")
        db.execute("INSERT INTO enhanced_chunks SELECT 'raw','old','ns.wav',160,engine FROM audio_enhancement")
        with self.assertRaises(sqlite3.IntegrityError):
            db.execute("INSERT INTO aec_intervals VALUES('raw',4,160,'bypass','timing_unavailable',NULL)")
        db.execute("INSERT INTO aec_intervals VALUES('raw',5,165,'bypass','reference_missing',NULL)")
        db.execute("UPDATE meetings SET state='recording' WHERE id='old'")
        with self.assertRaises(sqlite3.IntegrityError):
            db.execute("UPDATE audio_enhancement SET echo_cancellation=1 WHERE meeting_id='old'")
        db.execute("DELETE FROM meetings WHERE id='old'")
        self.assertEqual(db.execute('SELECT count(*) FROM audio_chunk_timing').fetchone()[0], 0)
        self.assertEqual(db.execute('SELECT count(*) FROM aec_intervals').fetchone()[0], 0)
        self.assertEqual(db.execute('PRAGMA foreign_key_check').fetchall(), [])
        db.close()


class ProductWorkspaceMigration(unittest.TestCase):
    def setUp(self):
        self.db = sqlite3.connect(':memory:')
        self.db.execute('PRAGMA foreign_keys=ON')
        for migration in sorted(MIGRATION.parent.glob('*.sql')):
            if migration.name < '0010':
                self.db.executescript(migration.read_text())
        self.db.execute("INSERT INTO meetings(id,title,timezone,created_at,analysis_mode,context_consent) VALUES ('old','old','Asia/Shanghai','now','transcript_only',0)")
        self.db.executescript((MIGRATION.parent/'0010_product_workspace.sql').read_text())

    def tearDown(self):
        self.db.close()

    def test_upgrade_keeps_existing_explicit_analysis_off(self):
        self.assertEqual(self.db.execute("SELECT analysis_mode,context_consent,analysis_choice FROM meetings WHERE id='old'").fetchone(), ('transcript_only', 0, 'legacy'))
        self.assertEqual(self.db.execute('PRAGMA user_version').fetchone()[0], 10)
        self.assertEqual(self.db.execute('PRAGMA foreign_key_check').fetchall(), [])

    def test_bound_knowledge_versions_are_retained_and_references_are_real(self):
        self.db.execute("INSERT INTO knowledge_sets VALUES ('k','词库','terms',1,0,'now')")
        self.db.execute("INSERT INTO knowledge_versions VALUES ('k',1,'词库','{}','now')")
        self.db.execute("INSERT INTO meeting_knowledge_bindings VALUES ('old','k',1)")
        with self.assertRaises(sqlite3.IntegrityError):
            self.db.execute("UPDATE meeting_knowledge_bindings SET version=2")
        for sql in ('DELETE FROM knowledge_versions', "UPDATE knowledge_versions SET payload_json='{}'"):
            with self.assertRaises(sqlite3.IntegrityError):
                self.db.execute(sql)
        self.db.execute("DELETE FROM meetings WHERE id='old'")
        self.assertEqual(self.db.execute('SELECT count(*) FROM meeting_knowledge_bindings').fetchone()[0], 0)
        self.assertEqual(self.db.execute('SELECT count(*) FROM knowledge_versions').fetchone()[0], 1)
        self.assertEqual(self.db.execute('PRAGMA foreign_key_check').fetchall(), [])

class DuozhouMigration(unittest.TestCase):
    def setUp(self):
        self.db=sqlite3.connect(':memory:'); self.db.execute('PRAGMA foreign_keys=ON')
        for migration in sorted(MIGRATION.parent.glob('*.sql')):
            if migration.name<'0011': self.db.executescript(migration.read_text())
        self.db.execute("INSERT INTO meetings(id,title,timezone,created_at) VALUES ('m','旧会议','Asia/Shanghai','now')")
        self.db.execute("INSERT INTO analysis_jobs(id,meeting_id,idempotency_key,type,input_snapshot,generation,state,attempts) VALUES ('j','m','legacy','qa','{}',1,'succeeded',1)")
        self.db.execute("INSERT INTO conversation_turns(job_id,meeting_id,epoch,question,scope,created_at) VALUES ('j','m',0,'旧问题','all','now')")
        self.db.executescript((MIGRATION.parent/'0011_duozhou_assistant.sql').read_text())
    def tearDown(self): self.db.close()
    def test_old_qa_migrates_without_regranting_permissions(self):
        self.assertEqual(self.db.execute("SELECT conversation_id,question,source,state,legacy_job_id FROM assistant_turns").fetchone(),('m','旧问题','text','succeeded','j'))
        self.assertEqual(self.db.execute("SELECT analysis_mode FROM meetings WHERE id='m'").fetchone()[0],'transcript_only')
        self.assertFalse(__import__('json').loads(self.db.execute("SELECT value FROM app_preferences WHERE key='assistant_settings'").fetchone()[0])['voice_enabled'])
    def test_source_dedup_active_turn_and_cascade(self):
        self.db.execute("INSERT INTO assistant_turns(id,conversation_id,epoch,question,source,utterance_id,state,created_at) VALUES ('a','m',0,'问题','voice','u','running','now')")
        for sql in ["INSERT INTO assistant_turns(id,conversation_id,epoch,question,source,state,created_at) VALUES ('b','m',0,'问题2','text','pending','now')", "INSERT INTO assistant_turns(id,conversation_id,epoch,question,source,utterance_id,state,created_at) VALUES ('b','m',0,'问题2','voice','u','succeeded','now')"]:
            with self.assertRaises(sqlite3.IntegrityError): self.db.execute(sql)
        self.db.execute("INSERT INTO assistant_proposals(id,turn_id,payload_json) VALUES ('p','a','{}')")
        self.db.execute("INSERT INTO assistant_playback_events(id,meeting_id,turn_id,text,start_host_ns) VALUES ('e','m','a','播报',0)")
        self.db.execute("DELETE FROM meetings WHERE id='m'")
        for table in ['assistant_conversations','assistant_turns','assistant_proposals','assistant_playback_events']:
            self.assertEqual(self.db.execute('SELECT count(*) FROM '+table).fetchone()[0],0)
        self.assertEqual(self.db.execute('PRAGMA foreign_key_check').fetchall(),[])

class AssistantBehaviorMigration(unittest.TestCase):
    def test_v20_upgrade_preserves_preferences_and_history(self):
        import json
        db=sqlite3.connect(':memory:')
        db.execute('PRAGMA foreign_keys=ON')
        for migration in sorted(MIGRATION.parent.glob('*.sql')):
            if migration.name < '0021': db.executescript(migration.read_text())
        db.execute("INSERT INTO meetings(id,title,timezone,created_at) VALUES ('m','synthetic','Asia/Shanghai','now')")
        db.execute("INSERT INTO assistant_conversations VALUES ('c','m','synthetic',0,'now')")
        db.execute("INSERT INTO assistant_turns(id,conversation_id,epoch,question,source,state,response_json,created_at) VALUES ('t','c',0,'synthetic','voice','succeeded','{}','now')")
        db.execute("UPDATE app_preferences SET value='1' WHERE key='debug_mode'")
        old=db.execute("SELECT key,value FROM app_preferences ORDER BY key").fetchall()
        db.commit()
        db.executescript((MIGRATION.parent/'0021_assistant_behavior.sql').read_text())
        self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0],21)
        self.assertEqual(db.execute("SELECT key,value FROM app_preferences WHERE key!='assistant_behavior_version' ORDER BY key").fetchall(),old)
        self.assertEqual(db.execute("SELECT response_json,speech_interrupted FROM assistant_turns WHERE id='t'").fetchone(),('{}',0))
        policy=json.loads(db.execute('SELECT value FROM assistant_behavior_versions').fetchone()[0])
        self.assertEqual([policy[k] for k in ('simple_seconds','medium_seconds','complex_seconds')],[10,20,30])
        for sql in ('DELETE FROM assistant_behavior_versions',"UPDATE assistant_behavior_versions SET value='{}'"):
            with self.assertRaises(sqlite3.IntegrityError): db.execute(sql)
        self.assertEqual(db.execute('PRAGMA foreign_key_check').fetchall(),[])
        db.close()


class PersonaDimensionsMigration(unittest.TestCase):
    def upgrade(self, custom):
        import json
        db=sqlite3.connect(':memory:')
        db.execute('PRAGMA foreign_keys=ON')
        for migration in sorted(MIGRATION.parent.glob('*.sql')):
            if migration.name < '0022': db.executescript(migration.read_text())
        policy=json.loads(db.execute('SELECT value FROM assistant_behavior_versions WHERE version=1').fetchone()[0])
        if custom:
            policy['persona_prompt']='保留我的自定义经历和口吻'
            policy['complex_seconds']=45
            db.execute("INSERT INTO assistant_behavior_versions VALUES (5,?,'synthetic')",(json.dumps(policy,ensure_ascii=False),))
            db.execute("UPDATE app_preferences SET value='5' WHERE key='assistant_behavior_version'")
        old=db.execute('SELECT version,value,created_at FROM assistant_behavior_versions ORDER BY version').fetchall()
        db.execute("INSERT INTO meetings(id,title,timezone,created_at) VALUES ('m','synthetic','Asia/Shanghai','now')")
        db.execute("INSERT INTO assistant_conversations VALUES ('c','m','synthetic',0,'now')")
        db.execute("INSERT INTO assistant_turns(id,conversation_id,epoch,question,source,state,snapshot_json,created_at) VALUES ('t','c',0,'synthetic','text','succeeded',?,'now')",(json.dumps({'context':{'behavior':policy}}),))
        before=db.execute("SELECT snapshot_json FROM assistant_turns WHERE id='t'").fetchone()[0]
        preferences=db.execute("SELECT key,value FROM app_preferences WHERE key!='assistant_behavior_version' ORDER BY key").fetchall()
        db.commit()
        db.executescript((MIGRATION.parent/'0022_persona_dimensions.sql').read_text())
        self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0],22)
        version=int(db.execute("SELECT value FROM app_preferences WHERE key='assistant_behavior_version'").fetchone()[0])
        self.assertEqual(version,6 if custom else 2)
        for row in old:
            self.assertEqual(db.execute('SELECT version,value,created_at FROM assistant_behavior_versions WHERE version=?',(row[0],)).fetchone(),row)
        new=json.loads(db.execute('SELECT value FROM assistant_behavior_versions WHERE version=?',(version,)).fetchone()[0])
        self.assertEqual(set(new['persona']),{'personality','education','expression','catchphrases','thinking_models','expertise'})
        self.assertEqual(new['persona_prompt'],policy['persona_prompt'] if custom else '')
        self.assertEqual(new['complex_seconds'],policy['complex_seconds'])
        self.assertEqual(db.execute("SELECT snapshot_json FROM assistant_turns WHERE id='t'").fetchone()[0],before)
        self.assertEqual(db.execute("SELECT key,value FROM app_preferences WHERE key!='assistant_behavior_version' ORDER BY key").fetchall(),preferences)
        self.assertEqual(db.execute('PRAGMA foreign_key_check').fetchall(),[])
        db.close()

    def test_upgrade_default_to_dimensions_without_rewriting_history(self): self.upgrade(False)
    def test_upgrade_custom_prompt_preserves_text_timing_and_versions(self): self.upgrade(True)


class PersonaConsolidationMigration(unittest.TestCase):
    def test_v22_merges_four_fields_without_losing_custom_text_or_history(self):
        import json
        for mode in ('default','custom','empty','duplicate','maximum'):
            with self.subTest(mode=mode):
                db=sqlite3.connect(':memory:')
                db.execute('PRAGMA foreign_keys=ON')
                for migration in sorted(MIGRATION.parent.glob('*.sql')):
                    if migration.name < '0023': db.executescript(migration.read_text())
                policy=json.loads(db.execute('SELECT value FROM assistant_behavior_versions WHERE version=2').fetchone()[0])
                if mode=='custom':
                    policy['persona'].update(expression='先给判断，再给一个例子。',catchphrases='先看关键点',expertise='研发协作经验',education='自定义跨学科背景')
                elif mode=='empty':
                    policy['persona'].update(expression='',catchphrases='',expertise='',education='')
                elif mode=='duplicate':
                    policy['persona'].update(expression='短句；先看关键点',catchphrases='先看关键点',expertise='研发实践；自定义背景',education='自定义背景')
                elif mode=='maximum':
                    policy['persona'].update(expression='风'*400,catchphrases='语'*120,expertise='经'*600,education='背'*500)
                policy['persona_prompt']='保留补充文本'
                policy['medium_seconds']=25
                db.execute("INSERT INTO assistant_behavior_versions VALUES (7,?,'synthetic')",(json.dumps(policy,ensure_ascii=False),))
                db.execute("UPDATE app_preferences SET value='7' WHERE key='assistant_behavior_version'")
                versions=db.execute('SELECT version,value,created_at FROM assistant_behavior_versions ORDER BY version').fetchall()
                db.execute("INSERT INTO meetings(id,title,timezone,created_at) VALUES ('m','synthetic','Asia/Shanghai','now')")
                db.execute("INSERT INTO assistant_conversations VALUES ('c','m','synthetic',0,'now')")
                frozen=json.dumps({'context':{'behavior':policy}},ensure_ascii=False)
                db.execute("INSERT INTO assistant_turns(id,conversation_id,epoch,question,source,state,snapshot_json,created_at) VALUES ('t','c',0,'synthetic','text','succeeded',?,'now')",(frozen,))
                prefs=db.execute("SELECT key,value FROM app_preferences WHERE key!='assistant_behavior_version' ORDER BY key").fetchall()
                db.commit()
                db.executescript((MIGRATION.parent/'0023_persona_consolidation.sql').read_text())
                self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0],23)
                self.assertEqual(db.execute("SELECT value FROM app_preferences WHERE key='assistant_behavior_version'").fetchone()[0],'8')
                new=json.loads(db.execute('SELECT value FROM assistant_behavior_versions WHERE version=8').fetchone()[0])
                persona=new['persona']; previous=policy['persona']
                self.assertEqual(set(persona),{'personality','expression','thinking_models','expertise'})
                self.assertEqual(new['persona_prompt'],policy['persona_prompt'])
                self.assertEqual(new['medium_seconds'],25)
                self.assertEqual(persona['personality'],previous['personality'])
                self.assertEqual(persona['thinking_models'],previous['thinking_models'])
                if mode=='default':
                    self.assertIn(previous['catchphrases'],persona['expression'])
                    self.assertNotIn(previous['education'],persona['expertise'])
                    self.assertNotIn('管理学、组织行为学',persona['expertise'])
                elif mode=='empty':
                    self.assertEqual(persona['expression'],'')
                    self.assertEqual(persona['expertise'],'')
                else:
                    self.assertIn(previous['expression'],persona['expression'])
                    self.assertIn(previous['expertise'],persona['expertise'])
                    self.assertEqual(persona['expression'].count(previous['catchphrases']),1)
                    self.assertEqual(persona['expertise'].count(previous['education']),1)
                self.assertLessEqual(len(persona['expression']),800)
                self.assertLessEqual(len(persona['expertise']),1200)
                self.assertEqual(db.execute('SELECT version,value,created_at FROM assistant_behavior_versions WHERE version<8 ORDER BY version').fetchall(),versions)
                self.assertEqual(db.execute("SELECT snapshot_json FROM assistant_turns WHERE id='t'").fetchone()[0],frozen)
                self.assertEqual(db.execute("SELECT key,value FROM app_preferences WHERE key!='assistant_behavior_version' ORDER BY key").fetchall(),prefs)
                self.assertEqual(db.execute('PRAGMA foreign_key_check').fetchall(),[])
                db.close()

class ComposedAssistantMigration(unittest.TestCase):
    def test_upgrade_adds_common_rules_and_versioned_guidance_without_rewriting_history(self):
        import json
        db=sqlite3.connect(':memory:');db.execute('PRAGMA foreign_keys=ON')
        for migration in sorted(MIGRATION.parent.glob('*.sql')):
            if migration.name<'0024':db.executescript(migration.read_text())
        db.execute("INSERT INTO assistant_templates VALUES ('custom',1,'synthetic','保留纪要规则')")
        db.execute("INSERT INTO template_versions VALUES ('custom',1,'synthetic','保留纪要规则')")
        version=int(db.execute("SELECT value FROM app_preferences WHERE key='assistant_behavior_version'").fetchone()[0])
        old=json.loads(db.execute('SELECT value FROM assistant_behavior_versions WHERE version=?',(version,)).fetchone()[0]);old['persona_prompt']='保留自定义通用要求';old['simple_seconds']=8
        db.execute("INSERT INTO assistant_behavior_versions VALUES (9,?,'synthetic')",(json.dumps(old,ensure_ascii=False),));db.execute("UPDATE app_preferences SET value='9' WHERE key='assistant_behavior_version'")
        versions=db.execute('SELECT * FROM assistant_behavior_versions ORDER BY version').fetchall();templates=db.execute('SELECT * FROM template_versions').fetchall();db.commit()
        db.executescript((MIGRATION.parent/'0024_composed_assistant.sql').read_text())
        new=json.loads(db.execute('SELECT value FROM assistant_behavior_versions WHERE version=10').fetchone()[0])
        self.assertIn('保留自定义通用要求',new['persona_prompt']);self.assertIn('不替参会人宣布决定',new['persona_prompt'])
        self.assertEqual(new['persona'],old['persona']);self.assertEqual(new['simple_seconds'],8)
        self.assertEqual(db.execute('SELECT * FROM assistant_behavior_versions WHERE version<=9 ORDER BY version').fetchall(),versions)
        self.assertEqual(db.execute('SELECT * FROM template_versions').fetchall(),templates)
        rules=json.loads(db.execute("SELECT policy_json FROM template_guidance WHERE template_id='custom'").fetchone()[0]);self.assertEqual(rules['criteria'],'保留纪要规则')
        self.assertEqual(set(rules),{'goal','criteria','qa_rules'})
        for sql in ("UPDATE template_guidance SET policy_json='{}'",'DELETE FROM template_guidance',"INSERT INTO template_guidance VALUES ('missing',1,'{}')"):
            with self.assertRaises(sqlite3.IntegrityError):db.execute(sql)
        self.assertEqual(db.execute('PRAGMA foreign_key_check').fetchall(),[])
        self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0],24)
        db.close()
