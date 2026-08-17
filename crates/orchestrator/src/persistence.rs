use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{de::DeserializeOwned, Serialize};

use crate::Result;

pub trait StateStore {
    fn save<T: Serialize>(&self, name: &str, value: &T) -> Result<()>;
    fn load<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>>;
}

pub struct JsonStateStore {
    directory: PathBuf,
}

impl JsonStateStore {
    pub fn new(directory: impl Into<PathBuf>) -> Result<Self> {
        let directory = directory.into();
        fs::create_dir_all(&directory)?;
        Ok(Self { directory })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.join(format!("{name}.json"))
    }
}

impl StateStore for JsonStateStore {
    fn save<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        let path = self.path(name);
        let temporary = path.with_extension("json.tmp");
        let mut file = File::create(&temporary)?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(temporary, path)?;
        Ok(())
    }

    fn load<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>> {
        let path = self.path(name);
        if !Path::new(&path).exists() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_reader(File::open(path)?)?))
    }
}
