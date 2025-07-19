use std::fmt::Debug;
use std::path::Path;

use anyhow::Result;
use bincode::{Decode, Encode};
use tokio::{
    fs::{self, File, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
};

#[derive(Debug)]
pub struct Cache {
    path: String,
}

impl Cache {
    pub fn new(path: &str) -> Self {
        Cache { path: path.into() }
    }

    pub async fn set<T: Encode>(&self, item: T) -> Result<()> {
        let path = Path::new(&self.path);
        if let Some(parent) = path.parent() {
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

    pub async fn get<T: Decode<()> + Debug>(&self) -> Result<Option<T>> {
        let path = Path::new(&self.path);
        if !path.exists() {
            return Ok(None);
        }
        let mut file = File::open(&self.path).await?;
        let mut buff = Vec::new();
        file.read_to_end(&mut buff).await?;
        let (item, _) = bincode::decode_from_slice(&buff, bincode::config::standard())?;
        Ok(Some(item))
    }
}
