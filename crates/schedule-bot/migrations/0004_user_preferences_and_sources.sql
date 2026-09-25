ALTER TABLE users
    ADD COLUMN daily_notifications_enabled BOOLEAN NOT NULL DEFAULT TRUE;

ALTER TABLE schedule_versions
    ADD COLUMN source_object_key TEXT;

ALTER TABLE pending_uploads
    ADD COLUMN source_object_key TEXT;
