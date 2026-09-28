CREATE TABLE schedule_site_state (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    semester_key TEXT NOT NULL,
    academic_week INTEGER NOT NULL CHECK (academic_week > 0),
    file_name TEXT NOT NULL,
    file_url TEXT NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE schedule_site_check_slots (
    slot_start TIMESTAMPTZ PRIMARY KEY,
    claimed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE schedule_reminder_weeks (
    week_start DATE PRIMARY KEY,
    academic_week INTEGER NOT NULL,
    file_name TEXT NOT NULL,
    file_url TEXT NOT NULL,
    queued_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
