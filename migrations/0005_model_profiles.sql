BEGIN IMMEDIATE;
CREATE TABLE model_profiles (
    id TEXT PRIMARY KEY,
    current_version INTEGER NOT NULL CHECK(current_version > 0),
    archived INTEGER NOT NULL DEFAULT 0 CHECK(archived IN (0,1)),
    FOREIGN KEY(id,current_version) REFERENCES model_configs(id,version)
);
CREATE TABLE app_preferences (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE meeting_model_bindings (
    meeting_id TEXT PRIMARY KEY REFERENCES meetings(id) ON DELETE CASCADE,
    profile_id TEXT NOT NULL,
    config_version INTEGER NOT NULL,
    FOREIGN KEY(profile_id,config_version) REFERENCES model_configs(id,version)
);
CREATE TABLE revoked_credentials (credential_ref TEXT PRIMARY KEY);
CREATE TABLE credential_cleanup (credential_ref TEXT PRIMARY KEY);
CREATE TABLE credential_pending (credential_ref TEXT PRIMARY KEY);
ALTER TABLE meetings ADD COLUMN consent_version INTEGER NOT NULL DEFAULT 0;
CREATE TRIGGER model_config_immutable BEFORE UPDATE ON model_configs
BEGIN SELECT RAISE(ABORT,'model versions are immutable'); END;
CREATE TRIGGER model_config_no_delete BEFORE DELETE ON model_configs
BEGIN SELECT RAISE(ABORT,'model history cannot be deleted'); END;
PRAGMA user_version = 5;
COMMIT;
