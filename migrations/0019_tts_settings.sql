BEGIN IMMEDIATE;

INSERT OR IGNORE INTO app_preferences(key,value) VALUES (
  'tts_settings',
  '{"enabled":false,"profile_id":null,"config_version":null,"model":"seed-tts-2.0-standard","speaker":"zh_female_tianmeitaozi_mars_bigtts","fallback_local":true}'
);

PRAGMA user_version=19;
COMMIT;
