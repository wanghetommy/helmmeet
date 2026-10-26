BEGIN IMMEDIATE;

-- The first TTS UI default used a Mars voice that is not available on the
-- Agent Plan seed-tts-2.0 resource. Migrate only that generated default;
-- preserve any user-selected speaker.
UPDATE app_preferences
SET value=json_set(value, '$.speaker', 'zh_female_gaolengyujie_uranus_bigtts')
WHERE key='tts_settings'
  AND json_extract(value, '$.speaker')='zh_female_tianmeitaozi_mars_bigtts';

PRAGMA user_version=20;
COMMIT;
