//! A read-only whole-file mapping with the unsafe boundary in one place.
//!
//! Native builds use `memmap2`; wasm builds read the file into memory (OPFS
//! files have no address space to map). The public API never exposes a
//! mutable slice, and the mapping is dropped with the `MappedFile`.
//!
//! Safety contract for callers: the file must not be modified or truncated
//! while a `MappedFile` for it is alive. `RegionFile` only maps files opened
//! read-only, and opens a separate writable handle for edits, so the map is
//! never used alongside writes made through this crate.
#![deny(unsafe_op_in_unsafe_fn)]

use std::{
    io,
    path::{Path, PathBuf},
};

#[cfg(not(target_arch = "wasm32"))]
use memmap2::{Mmap, MmapOptions};

/// A read-only file's bytes, mapped natively or held in memory on wasm.
pub struct MappedFile {
    path: PathBuf,
    bytes: Bytes,
}

enum Bytes {
    #[cfg(not(target_arch = "wasm32"))]
    Map(Mmap),
    Owned(Vec<u8>),
}

impl MappedFile {
    /// Opens `path` and maps it read-only. An empty file maps to an empty slice.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_owned();
        let file = std::fs::File::open(&path)?;
        let len = file.metadata()?.len();
        if len == 0 {
            return Ok(Self {
                path,
                bytes: Bytes::Owned(Vec::new()),
            });
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            // SAFETY: `file` is opened read-only and `map` owns the mapping for
            // as long as this `MappedFile`. A private copy-on-write mapping keeps
            // bytes stable if the region is written through another handle; the
            // documented caller contract still forbids truncating the file while
            // the mapping exists.
            let map = unsafe { MmapOptions::new().map_copy_read_only(&file)? };
            return Ok(Self {
                path,
                bytes: Bytes::Map(map),
            });
        }
        #[cfg(target_arch = "wasm32")]
        {
            let mut bytes = Vec::with_capacity(len as usize);
            std::io::Read::read_to_end(&mut &file, &mut bytes)?;
            Ok(Self {
                path,
                bytes: Bytes::Owned(bytes),
            })
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn as_slice(&self) -> &[u8] {
        match &self.bytes {
            #[cfg(not(target_arch = "wasm32"))]
            Bytes::Map(map) => map,
            Bytes::Owned(bytes) => bytes,
        }
    }

    pub fn len(&self) -> usize {
        self.as_slice().len()
    }

    pub fn is_empty(&self) -> bool {
        self.as_slice().is_empty()
    }
}

impl AsRef<[u8]> for MappedFile {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl std::fmt::Debug for MappedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MappedFile")
            .field("path", &self.path)
            .field("len", &self.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::MappedFile;
    use std::io::Write;

    fn temp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("horizon-mmap-{}-{name}", std::process::id()))
    }

    #[test]
    fn maps_the_whole_file_read_only() {
        let path = temp("whole");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"region bytes").unwrap();
        drop(f);
        let map = MappedFile::open(&path).unwrap();
        assert_eq!(map.as_slice(), b"region bytes");
        assert_eq!(map.len(), 12);
        assert!(!map.is_empty());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn empty_files_are_empty_not_errors() {
        let path = temp("empty");
        std::fs::File::create(&path).unwrap();
        let map = MappedFile::open(&path).unwrap();
        assert!(map.is_empty());
        assert_eq!(map.as_slice(), b"");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn missing_files_report_io_errors() {
        let path = temp("missing");
        let _ = std::fs::remove_file(&path);
        assert!(MappedFile::open(&path).is_err());
    }
}
