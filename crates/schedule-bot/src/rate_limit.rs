use anyhow::{Context, Result};
use redis::{AsyncCommands, aio::ConnectionManager};

const WINDOW_MS: u64 = 10_000;
const USER_LIMIT: u64 = 6;
const CHAT_LIMIT: u64 = 24;
const WARNING_COOLDOWN_MS: u64 = 30_000;

#[derive(Clone)]
pub struct RateLimiter {
    redis: ConnectionManager,
}

impl RateLimiter {
    pub async fn connect(redis_url: &str) -> Result<Self> {
        let client = redis::Client::open(redis_url).context("неверный REDIS_URL")?;
        let redis = client
            .get_connection_manager()
            .await
            .context("не удалось подключиться к Redis")?;
        Ok(Self { redis })
    }

    pub async fn allow(&self, user_id: i64, chat_id: i64) -> Result<bool> {
        let script = redis::Script::new(
            "local t=redis.call('TIME'); \
             local now=t[1]*1000+math.floor(t[2]/1000); \
             local window=tonumber(ARGV[1]); \
             for i,key in ipairs(KEYS) do redis.call('ZREMRANGEBYSCORE',key,'-inf',now-window); end; \
             local uc=redis.call('ZCARD',KEYS[1]); local cc=redis.call('ZCARD',KEYS[2]); \
             if uc>=tonumber(ARGV[2]) or cc>=tonumber(ARGV[3]) then return 0 end; \
             redis.call('ZADD',KEYS[1],now,ARGV[4]); redis.call('ZADD',KEYS[2],now,ARGV[4]); \
             redis.call('PEXPIRE',KEYS[1],window*2); redis.call('PEXPIRE',KEYS[2],window*2); return 1",
        );
        let mut redis = self.redis.clone();
        let allowed: i64 = script
            .key(format!("schedule:limit:user:{user_id}"))
            .key(format!("schedule:limit:chat:{chat_id}"))
            .arg(WINDOW_MS)
            .arg(USER_LIMIT)
            .arg(CHAT_LIMIT)
            .arg(uuid::Uuid::new_v4().to_string())
            .invoke_async(&mut redis)
            .await
            .context("ошибка Redis при проверке лимита запросов")?;
        Ok(allowed == 1)
    }

    /// Return true at most once per user during the warning cooldown.
    pub async fn should_warn(&self, user_id: i64) -> Result<bool> {
        let mut redis = self.redis.clone();
        let inserted: Option<String> = redis
            .set_options(
                format!("schedule:limit:warning:{user_id}"),
                "1",
                redis::SetOptions::default()
                    .conditional_set(redis::ExistenceCheck::NX)
                    .get(false)
                    .with_expiration(redis::SetExpiry::PX(WARNING_COOLDOWN_MS)),
            )
            .await
            .context("ошибка Redis при установке периода предупреждения")?;
        Ok(inserted.is_some())
    }

    pub async fn health(&self) -> Result<()> {
        let mut redis = self.redis.clone();
        let _: String = redis::cmd("PING")
            .query_async(&mut redis)
            .await
            .context("ошибка Redis health check")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn redis_limits_a_user_across_chats() -> Result<()> {
        let url = std::env::var("REDIS_URL")
            .context("REDIS_URL is required for this integration test")?;
        let limiter = RateLimiter::connect(&url).await?;
        let user = uuid::Uuid::new_v4().as_u128() as i64;
        for index in 0..USER_LIMIT {
            assert!(limiter.allow(user, user + index as i64 + 1).await?);
        }
        assert!(!limiter.allow(user, user + 100).await?);
        Ok(())
    }

    #[tokio::test]
    async fn redis_suppresses_repeat_warning_during_cooldown() -> Result<()> {
        let url = std::env::var("REDIS_URL")
            .context("REDIS_URL is required for this integration test")?;
        let limiter = RateLimiter::connect(&url).await?;
        let user = uuid::Uuid::new_v4().as_u128() as i64;
        assert!(limiter.should_warn(user).await?);
        assert!(!limiter.should_warn(user).await?);
        Ok(())
    }
}
