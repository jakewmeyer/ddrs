//! A binary cache system that provides persistent storage with integrity protection.
//!
//! This cache implementation stores serializable data in a binary format with checksums
//! to ensure data integrity. It uses atomic file operations to prevent corruption during writes.
//!
//! # Example
//! ```
//! # use anyhow::Result;
//! # use serde::{Serialize, Deserialize};
//! # async fn example() -> Result<()> {
//! # #[derive(Serialize, Deserialize)]
//! # struct MyData { value: String }
//!
//! let cache = Cache::new("/path/to/cache/dir");
//!
//! // Store data
//! let data = MyData { value: "example".to_string() };
//! cache.set(data).await?;
//!
//! // Retrieve data
//! let retrieved: Option<MyData> = cache.get().await?;
//! # Ok(())
//! # }
//! ```

use std::io::Cursor;
use std::path::PathBuf;
use std::{fmt::Debug, io::ErrorKind};

use anyhow::{Result, anyhow};
use crc32fast::Hasher;
use rmp_serde::Serializer;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tempfile::NamedTempFile;
use tokio::fs;
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncWriteExt},
};

/// Size of the binary header in bytes
const HEADER_SIZE: usize = 16;

/// Magic bytes to identify valid cache files
const MAGIC_IDENTIFIER: &[u8; 4] = b"DDRS";

/// Current cache format version
const VERSION: u16 = 1;

/// Reserved flags field (currently unused)
const FLAGS: u16 = 0;

/// Default filename for cache files
const DEFAULT_FILENAME: &str = "cache";

/// Default file extension for cache files
const DEFAULT_EXTENSION: &str = "ddrs";

/// A persistent cache implementation that stores serialized data with integrity protection.
///
/// The cache uses a binary format with checksums to ensure data integrity and
/// performs atomic file operations to prevent data corruption during writes.
///
/// # Binary Format
/// ```text
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                         Magic  (4 bytes)                      |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |        Version  (2 bytes)     |        Flags (2 bytes)        |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                     Data Length (4 bytes)                     |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                  Header Checksum (4 bytes)                    |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                                                               |
/// +                                                               +
/// |                                                               |
/// +                        Data (N bytes)                         +
/// |                                                               |
/// +                                                               +
/// |                                                               |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |                    Data Checksum (4 bytes)                    |
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// ```
#[derive(Debug)]
pub struct Cache {
    path: PathBuf,
}

impl Cache {
    /// Creates a new cache instance pointing to the specified directory
    pub fn new<P: Into<PathBuf>>(path: P) -> Self {
        let mut path_buf = path.into();
        path_buf.push(DEFAULT_FILENAME);
        path_buf.set_extension(DEFAULT_EXTENSION);
        Cache { path: path_buf }
    }

    /// Serializes and stores an item in the cache
    pub async fn set<T: Serialize>(&self, item: T) -> Result<()> {
        // Ensure parent directory exists
        let parent = self
            .path
            .parent()
            .ok_or(anyhow!("Invalid directory for cache file"))?;
        fs::create_dir_all(parent).await?;

        // Write to a temp file first
        let named_file = NamedTempFile::new_in(parent)?;
        let tmp_path = named_file.path().to_path_buf();
        let std_file = named_file.reopen()?;
        let mut file = File::from_std(std_file);

        // Build header + payload buffer
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(MAGIC_IDENTIFIER);
        buf.extend_from_slice(&VERSION.to_be_bytes());
        buf.extend_from_slice(&FLAGS.to_be_bytes());

        let mut data: Vec<u8> = Vec::new();
        item.serialize(&mut Serializer::new(&mut data).with_struct_map())?;

        let length = u32::try_from(data.len())?;
        buf.extend_from_slice(&length.to_be_bytes());

        // Write header checksum
        let mut hasher = Hasher::new();
        hasher.update(&buf);
        let checksum = hasher.finalize();
        buf.extend_from_slice(&checksum.to_be_bytes());

        // Write data + checksum
        buf.extend_from_slice(&data);
        let mut hasher = Hasher::new();
        hasher.update(&data);
        let checksum = hasher.finalize();
        buf.extend_from_slice(&checksum.to_be_bytes());

        // Write atomically
        file.write_all(&buf).await?;
        file.flush().await?;
        file.sync_all().await?;

        fs::rename(tmp_path, &self.path).await?;

        Ok(())
    }

    /// Retrieves and deserializes an item from the cache
    pub async fn get<T: DeserializeOwned>(&self) -> Result<Option<T>> {
        let mut file = match File::open(&self.path).await {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };

        let mut header_buffer = vec![0u8; HEADER_SIZE];
        if let Err(e) = file.read_exact(&mut header_buffer).await {
            if e.kind() == ErrorKind::UnexpectedEof {
                return Err(anyhow!(
                    "Cache file is truncated: could not read complete header"
                ));
            }
            return Err(e.into());
        }

        let mut cursor = Cursor::new(&header_buffer);

        let mut magic: [u8; 4] = [0; 4];
        cursor.read_exact(&mut magic).await?;
        if magic != *MAGIC_IDENTIFIER {
            return Err(anyhow!("Invalid magic header: {:#?}", magic));
        }

        let version = cursor.read_u16().await?;
        if version != VERSION {
            return Err(anyhow!("Invalid cache file version"));
        }

        let _flags = cursor.read_u16().await?;

        let data_length = cursor.read_u32().await?.try_into()?;

        let header_checksum = cursor.read_u32().await?;
        let mut hasher = Hasher::new();
        hasher.update(&header_buffer[0..12]);
        let calculated_header_checksum = hasher.finalize();
        if calculated_header_checksum != header_checksum {
            return Err(anyhow!(
                "Invalid cache file header checksum: Stored: {header_checksum} != Calculated: {calculated_header_checksum}"
            ));
        }

        let mut data: Vec<u8> = vec![0; data_length];
        file.read_exact(&mut data).await?;
        let data_checksum = file.read_u32().await?;
        let mut hasher = Hasher::new();
        hasher.update(&data);
        let calculated_checksum = hasher.finalize();
        if calculated_checksum != data_checksum {
            return Err(anyhow!(
                "Invalid cache file data checksum: Stored: {data_checksum} != Calculated: {calculated_checksum}"
            ));
        }
        let item = rmp_serde::from_slice(&data)?;

        Ok(Some(item))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use tempfile::tempdir;

    #[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
    struct TestData {
        id: u32,
        name: String,
        active: bool,
    }

    #[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
    struct Empty {}

    #[tokio::test]
    async fn test_cache_new() {
        let cache = Cache::new("/tmp/test_cache");
        assert_eq!(cache.path.to_str().unwrap(), "/tmp/test_cache/cache.ddrs");
    }

    #[tokio::test]
    async fn test_set_and_get() -> Result<()> {
        let cache = Cache::new(tempdir()?.path());

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
        let cache = Cache::new(tempdir()?.path());

        let result: Option<TestData> = cache.get().await?;
        assert_eq!(result, None);

        Ok(())
    }

    #[tokio::test]
    async fn test_set_creates_directory() -> Result<()> {
        let nested_path = tempdir()?.path().join("nested").join("dir");
        let cache = Cache::new(&nested_path);

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
        let cache = Cache::new(tempdir()?.path().to_str().unwrap());

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

    #[tokio::test]
    async fn test_get_empty_file() -> Result<()> {
        let cache = Cache::new(tempdir()?.path().to_str().unwrap());

        // Set initial data
        let initial_data = TestData {
            id: 1,
            name: "initial".to_string(),
            active: true,
        };
        cache.set(initial_data).await?;

        File::create(&cache.path).await?;

        let res: Result<Option<TestData>> = cache.get().await;
        assert!(res.is_err());
        let err = res.unwrap_err();
        assert_eq!(
            err.to_string(),
            "Cache file is truncated: could not read complete header"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_invalid_magic() -> Result<()> {
        let cache = Cache::new(tempdir()?.path());
        let td = TestData {
            id: 1,
            name: "x".into(),
            active: true,
        };
        cache.set(td.clone()).await?;

        let mut bytes = fs::read(&cache.path).await?;
        bytes[0..4].copy_from_slice(b"BAD!");
        fs::write(&cache.path, &bytes).await?;

        let res: Result<Option<TestData>> = cache.get().await;
        assert!(res.is_err());
        let err = res.unwrap_err();
        assert!(err.to_string().starts_with("Invalid magic header"));
        Ok(())
    }

    #[tokio::test]
    async fn test_invalid_version() -> Result<()> {
        let cache = Cache::new(tempdir()?.path());
        let td = TestData {
            id: 1,
            name: "x".into(),
            active: true,
        };
        cache.set(td.clone()).await?;

        let mut bytes = fs::read(&cache.path).await?;
        // Set version to 2
        bytes[4..6].copy_from_slice(&2u16.to_be_bytes());
        fs::write(&cache.path, &bytes).await?;

        let res: Result<Option<TestData>> = cache.get().await;
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().to_string(), "Invalid cache file version");
        Ok(())
    }

    #[tokio::test]
    async fn test_invalid_header_checksum() -> Result<()> {
        let cache = Cache::new(tempdir()?.path());
        let td = TestData {
            id: 1,
            name: "x".into(),
            active: true,
        };
        cache.set(td.clone()).await?;

        let mut bytes = fs::read(&cache.path).await?;
        // Corrupt only the header checksum field
        bytes[12] ^= 0xFF;
        fs::write(&cache.path, &bytes).await?;

        let res: Result<Option<TestData>> = cache.get().await;
        assert!(res.is_err());
        let msg = res.unwrap_err().to_string();
        assert!(msg.contains("Invalid cache file header checksum"));
        Ok(())
    }

    #[tokio::test]
    async fn test_invalid_data_checksum() -> Result<()> {
        let cache = Cache::new(tempdir()?.path());
        let td = TestData {
            id: 1,
            name: "x".into(),
            active: true,
        };
        cache.set(td.clone()).await?;

        let mut bytes = fs::read(&cache.path).await?;
        let mut len_bytes = [0u8; 4];
        len_bytes.copy_from_slice(&bytes[8..12]);
        let data_len = u32::from_be_bytes(len_bytes) as usize;
        let checksum_pos = HEADER_SIZE + data_len;
        // Flip a bit in the stored data checksum
        bytes[checksum_pos] ^= 0x01;
        fs::write(&cache.path, &bytes).await?;

        let res: Result<Option<TestData>> = cache.get().await;
        assert!(res.is_err());
        let msg = res.unwrap_err().to_string();
        assert!(msg.contains("Invalid cache file data checksum"));
        Ok(())
    }

    #[tokio::test]
    async fn test_empty_struct() -> Result<()> {
        let cache = Cache::new(tempdir()?.path());
        let v = Empty {};
        cache.set(v.clone()).await?;
        let got: Option<Empty> = cache.get().await?;
        assert_eq!(got, Some(v));
        Ok(())
    }

    #[tokio::test]
    async fn test_large_payload() -> Result<()> {
        let cache = Cache::new(tempdir()?.path());
        let big = "x".repeat(2 * 1024 * 1024); // 2 MiB
        let td = TestData {
            id: 7,
            name: big,
            active: true,
        };
        cache.set(td.clone()).await?;
        let got: Option<TestData> = cache.get().await?;
        assert_eq!(got, Some(td));
        Ok(())
    }
}
