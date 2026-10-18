BEGIN IMMEDIATE;
ALTER TABLE meetings ADD COLUMN context_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(context_json));
PRAGMA user_version = 4;
COMMIT;
