use anyhow::{Context, Result};
use object_store::{ObjectStore, aws::AmazonS3Builder, path::Path};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct SourceArchive {
    store: Arc<dyn ObjectStore>,
}

impl SourceArchive {
    pub fn from_env() -> Result<Self> {
        let value = |name: &str| {
            std::env::var(name).with_context(|| format!("не задана переменная окружения {name}"))
        };
        let store = AmazonS3Builder::new()
            .with_bucket_name(value("S3_BUCKET")?)
            .with_region(value("S3_REGION")?)
            .with_endpoint(value("S3_ENDPOINT")?)
            .with_access_key_id(value("S3_ACCESS_KEY_ID")?)
            .with_secret_access_key(value("S3_SECRET_ACCESS_KEY")?)
            .with_virtual_hosted_style_request(false)
            .build()
            .context("не удалось настроить S3 архив исходных таблиц")?;
        Ok(Self {
            store: Arc::new(store),
        })
    }

    pub async fn save(&self, upload_id: Uuid, extension: &str, contents: &[u8]) -> Result<String> {
        let key = format!("schedule-sources/{upload_id}.{extension}");
        self.store
            .put(&Path::from(key.clone()), contents.to_vec().into())
            .await
            .context("не удалось сохранить исходную таблицу в S3")?;
        Ok(key)
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        self.store
            .delete(&Path::from(key))
            .await
            .context("не удалось удалить исходную таблицу из S3")
    }
}
