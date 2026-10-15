use anyhow::{Context, Result, ensure};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Storage boundary for a future remote implementation. Values are original bytes.
pub trait BlobStore {
    fn put(&self, hash: &str, bytes: &[u8]) -> Result<u64>;
    fn get(&self, hash: &str) -> Result<Vec<u8>>;
    fn delete(&self, hash: &str) -> Result<()>;
    fn exists(&self, hash: &str) -> Result<bool>;
}

pub struct LocalStore {
    pub root: PathBuf,
    pub size_limit: u64,
}

pub(crate) fn directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    ensure!(!fs::symlink_metadata(path)?.file_type().is_symlink(), "storage directory must not be a symlink");
    Ok(())
}

pub(crate) fn reject_link(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) => ensure!(!m.file_type().is_symlink(), "storage file must not be a symlink"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("missing parent directory")?;
    directory(parent)?;
    reject_link(path)?;
    let mut temp = tempfile::Builder::new().prefix(".pending-").tempfile_in(parent)?;
    let middle = bytes.len() / 2;
    temp.write_all(&bytes[..middle])?;
    crate::fault("during_blob_write");
    temp.write_all(&bytes[middle..])?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

impl LocalStore {
    pub fn new(root: PathBuf, size_limit: u64) -> Result<Self> {
        directory(&root)?;
        Ok(Self { root, size_limit })
    }

    pub fn path(&self, hash: &str) -> Result<PathBuf> {
        ensure!(hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()), "invalid blob hash");
        let parent = self.root.join(&hash[..2]);
        if parent.exists() { reject_link(&parent)?; }
        Ok(parent.join(&hash[2..]))
    }
}

impl BlobStore for LocalStore {
    fn put(&self, hash: &str, bytes: &[u8]) -> Result<u64> {
        ensure!(bytes.len() as u64 <= self.size_limit, "blob exceeds file size limit");
        ensure!(blake3::hash(bytes).to_hex().as_str() == hash, "blob content does not match hash");
        let path = self.path(hash)?;
        reject_link(&path)?;
        if !path.exists() {
            let compressed = zstd::stream::encode_all(bytes, 3)?;
            atomic_write(&path, &compressed)?;
        } else {
            let _ = self.get(hash)?;
        }
        Ok(fs::metadata(path)?.len())
    }

    fn get(&self, hash: &str) -> Result<Vec<u8>> {
        use std::io::Read;
        let path = self.path(hash)?;
        reject_link(&path)?;
        let mut bytes = Vec::new();
        zstd::stream::read::Decoder::new(File::open(path)?)?
            .take(self.size_limit + 1).read_to_end(&mut bytes)?;
        ensure!(bytes.len() as u64 <= self.size_limit, "blob exceeds file size limit");
        ensure!(blake3::hash(&bytes).to_hex().as_str() == hash, "stored blob failed integrity verification");
        Ok(bytes)
    }

    fn delete(&self, hash: &str) -> Result<()> {
        let path = self.path(hash)?;
        reject_link(&path)?;
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn exists(&self, hash: &str) -> Result<bool> {
        let path = self.path(hash)?;
        reject_link(&path)?;
        Ok(path.is_file())
    }
}
