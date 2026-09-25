CREATE TABLE chat_schedules (
    chat_id BIGINT PRIMARY KEY,
    group_code TEXT NOT NULL,
    configured_by BIGINT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
