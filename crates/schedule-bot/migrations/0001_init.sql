CREATE TABLE users (
    telegram_id BIGINT PRIMARY KEY,
    chat_id BIGINT NOT NULL,
    username TEXT,
    role TEXT NOT NULL DEFAULT 'student' CHECK (role IN ('student', 'admin')),
    group_code TEXT,
    pending_group TEXT,
    flow_state TEXT NOT NULL DEFAULT 'start',
    search_kind TEXT CHECK (search_kind IN ('teacher', 'room')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE admin_bootstrap_claims (
    claim_key TEXT PRIMARY KEY CHECK (claim_key = 'primary'),
    telegram_id BIGINT NOT NULL REFERENCES users(telegram_id),
    claimed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE schedule_versions (
    id UUID PRIMARY KEY,
    week_start DATE NOT NULL,
    week_end DATE NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    update_kind TEXT NOT NULL CHECK (update_kind IN ('new_week', 'correction')),
    file_name TEXT NOT NULL,
    file_sha256 TEXT NOT NULL,
    created_by BIGINT NOT NULL REFERENCES users(telegram_id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (week_start, revision),
    CHECK (week_end = week_start + 6)
);

CREATE TABLE schedule_weeks (
    week_start DATE PRIMARY KEY,
    week_end DATE NOT NULL,
    current_version UUID NOT NULL REFERENCES schedule_versions(id),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (week_end = week_start + 6)
);

CREATE TABLE lessons (
    id BIGSERIAL PRIMARY KEY,
    version_id UUID NOT NULL REFERENCES schedule_versions(id) ON DELETE CASCADE,
    groups TEXT[] NOT NULL CHECK (cardinality(groups) > 0),
    lesson_date DATE NOT NULL,
    weekday TEXT NOT NULL,
    lesson_number SMALLINT NOT NULL,
    start_time TEXT NOT NULL,
    end_time TEXT NOT NULL,
    subject TEXT NOT NULL,
    lesson_type TEXT,
    teacher TEXT,
    room TEXT,
    description TEXT NOT NULL,
    CHECK (lesson_number > 0)
);

CREATE TABLE pending_uploads (
    id UUID PRIMARY KEY,
    uploaded_by BIGINT NOT NULL REFERENCES users(telegram_id),
    file_name TEXT NOT NULL,
    file_sha256 TEXT NOT NULL,
    week_start DATE NOT NULL,
    week_end DATE NOT NULL,
    update_kind TEXT NOT NULL CHECK (update_kind IN ('new_week', 'correction')),
    base_version UUID REFERENCES schedule_versions(id),
    diff JSONB NOT NULL,
    parsed_lessons JSONB NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'confirmed', 'cancelled')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    confirmed_at TIMESTAMPTZ
);

CREATE TABLE admin_invites (
    token_sha256 TEXT PRIMARY KEY,
    created_by BIGINT NOT NULL REFERENCES users(telegram_id),
    expires_at TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ,
    consumed_by BIGINT REFERENCES users(telegram_id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE notification_outbox (
    id BIGSERIAL PRIMARY KEY,
    telegram_id BIGINT NOT NULL REFERENCES users(telegram_id),
    body TEXT NOT NULL,
    available_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    claimed_at TIMESTAMPTZ,
    sent_at TIMESTAMPTZ,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT
);

CREATE INDEX lessons_group_date_idx ON lessons USING GIN (groups);
CREATE INDEX lessons_date_time_idx ON lessons (lesson_date, start_time);
CREATE INDEX lessons_room_idx ON lessons (room);
CREATE INDEX lessons_teacher_idx ON lessons (teacher);
CREATE INDEX pending_upload_status_idx ON pending_uploads (status, created_at);
CREATE INDEX outbox_ready_idx ON notification_outbox (available_at) WHERE sent_at IS NULL;
