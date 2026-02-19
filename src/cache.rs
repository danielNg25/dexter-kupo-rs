use anyhow::{anyhow, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::fs::File;
use std::io::{BufReader, BufWriter};

/// Save a serializable object to a JSON file.
pub fn save_to_file<T: Serialize>(data: &T, path: &str) -> Result<()> {
    let file =
        File::create(path).map_err(|e| anyhow!("Failed to create cache file {}: {}", path, e))?;
    let writer = BufWriter::new(file);
    serde_json::to_writer_pretty(writer, data)
        .map_err(|e| anyhow!("Failed to write cache to {}: {}", path, e))?;
    Ok(())
}

/// Load a deserializable object from a JSON file.
pub fn load_from_file<T: DeserializeOwned>(path: &str) -> Result<T> {
    let file =
        File::open(path).map_err(|e| anyhow!("Failed to open cache file {}: {}", path, e))?;
    let reader = BufReader::new(file);
    let data = serde_json::from_reader(reader)
        .map_err(|e| anyhow!("Failed to parse cache file {}: {}", path, e))?;
    Ok(data)
}
