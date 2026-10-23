BEGIN IMMEDIATE;

INSERT OR IGNORE INTO app_preferences(key,value) VALUES ('debug_mode','false');

PRAGMA user_version=18;
COMMIT;
