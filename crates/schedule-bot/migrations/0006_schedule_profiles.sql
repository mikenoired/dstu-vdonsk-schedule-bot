ALTER TABLE users
    ADD COLUMN schedule_profile TEXT CHECK (schedule_profile IN ('student', 'teacher')),
    ADD COLUMN teacher_name TEXT,
    ADD COLUMN pending_teacher TEXT;

UPDATE users
SET schedule_profile = 'student'
WHERE group_code IS NOT NULL OR pending_group IS NOT NULL;
