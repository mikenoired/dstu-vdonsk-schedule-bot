FROM rust:1-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p schedule-bot

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates fonts-dejavu-core && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/schedule-bot /usr/local/bin/schedule-bot
CMD ["schedule-bot"]
