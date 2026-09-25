use anyhow::{Context, Result};
use object_store::{ObjectStore, aws::AmazonS3Builder, path::Path};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct SourceArchive {
    store: Arc<dyn ObjectStore>,
}

impl SourceArchive {
    pub fn from_env() -> Result<Option<Self>> {
        let endpoint = env_any(&["AWS_ENDPOINT_URL", "S3_ENDPOINT", "ENDPOINT"]);
        let bucket = env_any(&["AWS_S3_BUCKET_NAME", "S3_BUCKET", "BUCKET"]);
        let region = env_any(&["AWS_DEFAULT_REGION", "S3_REGION", "REGION"]);
        let access_key = env_any(&["AWS_ACCESS_KEY_ID", "S3_ACCESS_KEY_ID", "ACCESS_KEY_ID"]);
        let secret_key = env_any(&[
            "AWS_SECRET_ACCESS_KEY",
            "S3_SECRET_ACCESS_KEY",
            "SECRET_ACCESS_KEY",
        ]);
        let values = [endpoint, bucket, region, access_key, secret_key];
        if values.iter().all(Option::is_none) {
            return Ok(None);
        }
        let [
            Some(endpoint),
            Some(bucket),
            Some(region),
            Some(access_key),
            Some(secret_key),
        ] = values
        else {
            anyhow::bail!(
                "S3 настроен не полностью: нужны endpoint, bucket, region, access key и secret key (Railway: AWS_ENDPOINT_URL, AWS_S3_BUCKET_NAME, AWS_DEFAULT_REGION, AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY)"
            );
        };
        let style = env_any(&["AWS_S3_URL_STYLE", "S3_URL_STYLE", "URL_STYLE"])
            .unwrap_or_else(|| "virtual".to_owned());
        let virtual_hosted = match style.to_ascii_lowercase().as_str() {
            "virtual" | "virtual-hosted" | "virtual_hosted" => true,
            "path" | "path-style" | "path_style" => false,
            other => {
                anyhow::bail!("неизвестный S3 URL style `{other}`; ожидается `virtual` или `path`")
            }
        };
        let store = AmazonS3Builder::new()
            .with_bucket_name(bucket)
            .with_region(region)
            .with_endpoint(endpoint)
            .with_access_key_id(access_key)
            .with_secret_access_key(secret_key)
            .with_virtual_hosted_style_request(virtual_hosted)
            .build()
            .context("не удалось настроить S3 архив исходных таблиц")?;
        Ok(Some(Self {
            store: Arc::new(store),
        }))
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

fn env_any(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    })
}
