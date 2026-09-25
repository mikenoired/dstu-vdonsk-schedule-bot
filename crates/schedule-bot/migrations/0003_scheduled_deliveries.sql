CREATE TABLE scheduled_deliveries (
    telegram_id BIGINT NOT NULL REFERENCES users(telegram_id) ON DELETE CASCADE,
    delivery_date DATE NOT NULL,
    delivery_kind TEXT NOT NULL CHECK (delivery_kind IN ('today', 'tomorrow')),
    queued_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (telegram_id, delivery_date, delivery_kind)
);
