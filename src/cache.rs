use std::{fmt::Debug, io::ErrorKind};
use std::path::PathBuf;

use anyhow::Result;
use bincode::{Decode, Encode};
use tokio::{
    fs::{self, File, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
};

#[derive(Debug)]
pub struct Cache {
    path: PathBuf,
}

impl Cache {
    pub fn new<P: Into<PathBuf>>(path: P) -> Self {
        Cache { path: path.into() }
    }

    pub async fn set<T: Encode>(&self, item: T) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .create(true)
            .open(&self.path)
            .await?;
        let buff = bincode::encode_to_vec(item, bincode::config::standard())?;
        file.write_all(&buff).await?;
        file.flush().await?;
        Ok(())
    }

    pub async fn get<T: Decode<()>>(&self) -> Result<Option<T>> {
        let mut file = match File::open(&self.path).await {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut buff = Vec::new();
        file.read_to_end(&mut buff).await?;
        let (item, _) = bincode::decode_from_slice(&buff, bincode::config::standard())?;
        Ok(Some(item))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bincode::{Decode, Encode};
    use tempfile::tempdir;

    #[derive(Debug, Encode, Decode, PartialEq, Clone)]
    struct TestData {
        id: u32,
        name: String,
        active: bool,
    }

    #[tokio::test]
    async fn test_cache_new() {
        let cache = Cache::new("/tmp/test_cache");
        assert_eq!(cache.path.to_str().unwrap(), "/tmp/test_cache");
    }

    #[tokio::test]
    async fn test_set_and_get() -> Result<()> {
        let temp_dir = tempdir()?;
        let cache_path = temp_dir.path().join("test_cache.bin");
        let cache = Cache::new(cache_path.to_str().unwrap());

        let test_data = TestData {
            id: 42,
            name: "test".to_string(),
            active: true,
        };

        // Set data
        cache.set(test_data.clone()).await?;

        // Get data back
        let retrieved: Option<TestData> = cache.get().await?;
        assert_eq!(retrieved, Some(test_data));

        Ok(())
    }

    #[tokio::test]
    async fn test_get_nonexistent_file() -> Result<()> {
        let temp_dir = tempdir()?;
        let cache_path = temp_dir.path().join("nonexistent.bin");
        let cache = Cache::new(cache_path.to_str().unwrap());

        let result: Option<TestData> = cache.get().await?;
        assert_eq!(result, None);

        Ok(())
    }

    #[tokio::test]
    async fn test_set_creates_directory() -> Result<()> {
        let temp_dir = tempdir()?;
        let nested_path = temp_dir.path().join("nested").join("dir").join("cache.bin");
        let cache = Cache::new(nested_path.to_str().unwrap());

        let test_data = TestData {
            id: 1,
            name: "nested".to_string(),
            active: false,
        };

        cache.set(test_data.clone()).await?;

        // Verify directory was created
        assert!(nested_path.parent().unwrap().exists());

        // Verify data can be retrieved
        let retrieved: Option<TestData> = cache.get().await?;
        assert_eq!(retrieved, Some(test_data));

        Ok(())
    }

    #[tokio::test]
    async fn test_set_overwrites_existing_file() -> Result<()> {
        let temp_dir = tempdir()?;
        let cache_path = temp_dir.path().join("overwrite_test.bin");
        let cache = Cache::new(cache_path.to_str().unwrap());

        // Set initial data
        let initial_data = TestData {
            id: 1,
            name: "initial".to_string(),
            active: true,
        };
        cache.set(initial_data).await?;

        // Overwrite with new data
        let new_data = TestData {
            id: 2,
            name: "overwritten".to_string(),
            active: false,
        };
        cache.set(new_data.clone()).await?;

        // Verify only new data is present
        let retrieved: Option<TestData> = cache.get().await?;
        assert_eq!(retrieved, Some(new_data));

        Ok(())
    }
}
