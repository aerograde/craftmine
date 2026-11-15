//! Files. On native, std's. In the browser, a virtual filesystem in memory:
//! the page installs the prepared resources as a tar archive before the game
//! starts ([`install_tar`]), files it lists in a manifest without their bytes
//! ([`install_manifest`]) are fetched from the page's origin the first time
//! they are read. What the game writes outside `/tmp` and `/.local` (options,
//! saves) is also kept in the browser's storage: the page loads it back before
//! the game starts ([`install_saved_file`]) and saves the changes
//! ([`take_changes`]).
//!
//! `Path::exists`, `is_dir` and `is_file` go to std's filesystem, which the
//! browser lacks: call [`exists`], [`is_dir`] and [`is_file`] instead.

#[cfg(not(target_arch = "wasm32"))]
pub use std::fs::*;

#[cfg(not(target_arch = "wasm32"))]
pub fn exists(path: impl AsRef<std::path::Path>) -> bool {
    path.as_ref().exists()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn is_dir(path: impl AsRef<std::path::Path>) -> bool {
    path.as_ref().is_dir()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn is_file(path: impl AsRef<std::path::Path>) -> bool {
    path.as_ref().is_file()
}

/// `Path::exists`, `is_dir`, `is_file` and `metadata` that see the web's
/// virtual filesystem (std's own see no files there).
pub trait PathExt {
    fn fs_exists(&self) -> bool;
    fn fs_is_dir(&self) -> bool;
    fn fs_is_file(&self) -> bool;
    fn fs_metadata(&self) -> std::io::Result<Metadata>;
    fn fs_canonicalize(&self) -> std::io::Result<std::path::PathBuf>;
}

impl PathExt for std::path::Path {
    fn fs_exists(&self) -> bool {
        exists(self)
    }
    fn fs_is_dir(&self) -> bool {
        is_dir(self)
    }
    fn fs_is_file(&self) -> bool {
        is_file(self)
    }
    fn fs_metadata(&self) -> std::io::Result<Metadata> {
        metadata(self)
    }
    fn fs_canonicalize(&self) -> std::io::Result<std::path::PathBuf> {
        canonicalize(self)
    }
}

#[cfg(target_arch = "wasm32")]
pub use web::*;

#[cfg(target_arch = "wasm32")]
mod web {
    use crate::time::SystemTime;
    use std::{
        collections::BTreeMap,
        ffi::OsString,
        io::{self, Read, Seek, SeekFrom, Write},
        path::{Component, Path, PathBuf},
        sync::{Arc, LazyLock, Mutex, MutexGuard},
        time::Duration,
    };
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen(inline_js = r#"
export function horizon_fetch_sync(url) {
    // The offline page (web/package.py) holds its files itself: a file:// page can't fetch.
    const held = globalThis.horizonHeldFile?.(url);
    if (held) return held;
    // The offline file's store does not hold every path its manifest names:
    // an unknown held file is missing, not a `held:` URL for XHR to fetch (it
    // answered with a zero-length file, which a sound decoder reads as empty).
    if (url.startsWith("held:")) return undefined;
    // The single-file build (web/package.py single) installs every resource and
    // sets this: a file that is missing is a bug, not a URL to request. Only
    // the game's explicit networking may reach the network there.
    if (globalThis.horizonOfflineFilesOnly) return undefined;
    const xhr = new XMLHttpRequest();
    xhr.open("GET", url, false);
    // Synchronous requests on the main thread can't ask for an ArrayBuffer:
    // read the bytes as a binary string instead.
    xhr.overrideMimeType("text/plain; charset=x-user-defined");
    xhr.send();
    if (xhr.status !== 200 && xhr.status !== 0) return undefined;
    const text = xhr.responseText;
    const bytes = new Uint8Array(text.length);
    for (let i = 0; i < text.length; i++) bytes[i] = text.charCodeAt(i) & 0xff;
    return bytes;
}
"#)]
    extern "C" {
        fn horizon_fetch_sync(url: &str) -> Option<Vec<u8>>;
    }

    /// Where files listed by the manifest are fetched from: the folder they
    /// were listed under, and the URL prefix their path below it goes after.
    static REMOTE: Mutex<(String, String)> = Mutex::new((String::new(), String::new()));

    #[derive(Clone)]
    enum Node {
        Dir,
        File {
            data: Vec<u8>,
            modified: SystemTime,
        },
        /// Listed by the manifest, fetched on first read: alone, or with the
        /// rest of its pack (a `.tar.gz` of many files, one request).
        Remote {
            len: u64,
            pack: Option<Arc<str>>,
        },
    }

    #[derive(Default)]
    struct Vfs {
        nodes: BTreeMap<String, Node>,
        /// Paths changed since the page last saved them, and whether a folder
        /// there is emptied first (it replaced another by a rename).
        dirty: BTreeMap<String, bool>,
    }

    /// Scratch files and the prepared resources aren't the player's: they stay
    /// out of the browser's storage.
    fn persisted(key: &str) -> bool {
        !["/tmp", "/.local"]
            .iter()
            .any(|root| key == *root || key.starts_with(&format!("{root}/")))
    }

    /// A path's state for the page to save, from [`take_changes`].
    pub enum Change {
        File(Vec<u8>),
        Dir { clear: bool },
        Gone,
    }

    static VFS: LazyLock<Mutex<Vfs>> = LazyLock::new(|| {
        let mut vfs = Vfs::default();
        vfs.nodes.insert("/".into(), Node::Dir);
        Mutex::new(vfs)
    });

    fn vfs() -> MutexGuard<'static, Vfs> {
        crate::sync::lock(&VFS).unwrap_or_else(|e| e.into_inner())
    }

    /// The packs asked for: a second reader of one waits for the first fetch
    /// instead of fetching it again; `true` once that fetch failed.
    static PACKS: Mutex<BTreeMap<String, Arc<Mutex<bool>>>> = Mutex::new(BTreeMap::new());

    /// Fetches a manifest file before it is read, without holding the
    /// filesystem's lock: another thread's reads go on during the request.
    fn prefetch(key: &str) {
        let pack = match vfs().nodes.get(key) {
            Some(Node::Remote {
                pack: Some(pack), ..
            }) => Some(pack.clone()),
            _ => None,
        };
        if let Some(pack) = pack {
            fetch_pack(&pack);
            return;
        }
        let Some(url) = remote_url(&vfs(), key) else {
            return;
        };
        if let Some(data) = horizon_fetch_sync(&url) {
            let mut vfs = vfs();
            if matches!(vfs.nodes.get(key), Some(Node::Remote { .. })) {
                vfs.nodes.insert(
                    key.to_owned(),
                    Node::File {
                        data,
                        modified: SystemTime::now(),
                    },
                );
            }
        }
    }

    /// Fetches the pack `rel` (a `.tar.gz`, from the manifest's base) once and
    /// installs its files that are still waiting to be fetched. After a
    /// failed fetch each of them is fetched alone when read.
    fn fetch_pack(rel: &str) {
        let gate = crate::sync::lock(&PACKS)
            .unwrap_or_else(|e| e.into_inner())
            .entry(rel.to_owned())
            .or_default()
            .clone();
        let mut failed = crate::sync::lock(&gate).unwrap_or_else(|e| e.into_inner());
        if *failed
            || !vfs()
                .nodes
                .values()
                .any(|n| matches!(n, Node::Remote { pack: Some(p), .. } if &**p == rel))
        {
            // Installed while this thread waited, or each file is fetched alone.
            return;
        }
        *failed = true;
        let (root, base) = crate::sync::lock(&REMOTE)
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let url = format!("{base}{rel}");
        let Some(gz) = horizon_fetch_sync(&url) else {
            return;
        };
        let mut tar = Vec::new();
        if let Err(e) = flate2::read::GzDecoder::new(&gz[..]).read_to_end(&mut tar) {
            crate::web::console(2, &format!("Horizon: {url}: {e}"));
            return;
        }
        match install_tar_files(&root, &tar, true) {
            Ok(_) => *failed = false,
            Err(e) => crate::web::console(2, &format!("Horizon: {url}: {e}")),
        }
    }

    /// Where a manifest file not fetched yet is fetched from.
    fn remote_url(vfs: &Vfs, key: &str) -> Option<String> {
        if !matches!(vfs.nodes.get(key), Some(Node::Remote { .. })) {
            return None;
        }
        let remote = crate::sync::lock(&REMOTE).unwrap_or_else(|e| e.into_inner());
        let rel = key.strip_prefix(&remote.0).unwrap_or(key);
        Some(format!("{}{}", remote.1, rel.trim_start_matches('/')))
    }

    /// `path` from the root, `.` and `..` resolved, no trailing slash.
    fn key(path: &Path) -> String {
        let mut parts: Vec<String> = Vec::new();
        for c in path.components() {
            match c {
                Component::RootDir | Component::Prefix(_) => parts.clear(),
                Component::CurDir => {}
                Component::ParentDir => {
                    parts.pop();
                }
                Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            }
        }
        format!("/{}", parts.join("/"))
    }

    fn parent_key(key: &str) -> Option<&str> {
        match key.rfind('/') {
            Some(0) if key.len() > 1 => Some("/"),
            Some(0) | None => None,
            Some(i) => Some(&key[..i]),
        }
    }

    fn not_found(path: &Path) -> io::Error {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("{}: no such file or directory", path.display()),
        )
    }

    impl Vfs {
        fn touch(&mut self, key: &str, clear: bool) {
            if !persisted(key) {
                return;
            }
            match self.dirty.get_mut(key) {
                Some(c) => *c |= clear,
                None => {
                    self.dirty.insert(key.to_owned(), clear);
                }
            }
        }
        fn mkdirs(&mut self, key: &str) {
            let mut at = key;
            let mut missing = Vec::new();
            while !self.nodes.contains_key(at) {
                missing.push(at.to_owned());
                match parent_key(at) {
                    Some(p) => at = p,
                    None => break,
                }
            }
            for k in missing {
                self.nodes.insert(k, Node::Dir);
            }
        }
        fn put(&mut self, key: String, node: Node) {
            if let Some(p) = parent_key(&key) {
                self.mkdirs(p);
            }
            self.nodes.insert(key, node);
        }
        /// The file's bytes, fetching a manifest file the first time.
        fn data(&mut self, key: &str, path: &Path) -> io::Result<&mut Vec<u8>> {
            match self.nodes.get(key) {
                None => return Err(not_found(path)),
                Some(Node::Dir) => {
                    return Err(io::Error::new(
                        io::ErrorKind::IsADirectory,
                        format!("{}: is a directory", path.display()),
                    ));
                }
                Some(Node::Remote { .. }) => {
                    let url = remote_url(self, key).unwrap_or_default();
                    let data = horizon_fetch_sync(&url).ok_or_else(|| {
                        io::Error::new(io::ErrorKind::NotFound, format!("could not fetch {url}"))
                    })?;
                    self.nodes.insert(
                        key.to_owned(),
                        Node::File {
                            data,
                            modified: SystemTime::now(),
                        },
                    );
                }
                Some(Node::File { .. }) => {}
            }
            match self.nodes.get_mut(key) {
                Some(Node::File { data, .. }) => Ok(data),
                _ => unreachable!(),
            }
        }
        fn children(&self, key: &str) -> Vec<String> {
            let prefix = if key == "/" {
                "/".to_owned()
            } else {
                format!("{key}/")
            };
            self.nodes
                .range(prefix.clone()..)
                .take_while(|(k, _)| k.starts_with(&prefix))
                .filter(|(k, _)| k.len() > prefix.len() && !k[prefix.len()..].contains('/'))
                .map(|(k, _)| k.clone())
                .collect()
        }
    }

    /// Adds every file of a tar archive (ustar, as `tar -c` writes it) under
    /// `root`.
    pub fn install_tar(root: &str, tar: &[u8]) -> io::Result<usize> {
        install_tar_files(root, tar, false)
    }

    /// [`install_tar`]; with `only_remote`, only the files still waiting to be
    /// fetched, so none the game wrote or already has is replaced.
    fn install_tar_files(root: &str, tar: &[u8], only_remote: bool) -> io::Result<usize> {
        let invalid = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_owned());
        let field = |b: &[u8]| {
            let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
            String::from_utf8_lossy(&b[..end]).into_owned()
        };
        let octal = |b: &[u8]| {
            let s = field(b);
            u64::from_str_radix(s.trim(), 8).map_err(|_| invalid("bad tar size"))
        };
        let mut vfs = vfs();
        let mut at = 0;
        let mut count = 0;
        let mut long_name = None;
        while at + 512 <= tar.len() {
            let header = &tar[at..at + 512];
            if header.iter().all(|&b| b == 0) {
                break;
            }
            let size = octal(&header[124..136])? as usize;
            let kind = header[156];
            let body = at + 512;
            if body + size > tar.len() {
                return Err(invalid("truncated tar"));
            }
            let content = &tar[body..body + size];
            let name = match long_name.take() {
                Some(name) => name,
                None => {
                    let name = field(&header[0..100]);
                    let prefix = field(&header[345..500]);
                    if prefix.is_empty() {
                        name
                    } else {
                        format!("{prefix}/{name}")
                    }
                }
            };
            match kind {
                b'L' => long_name = Some(field(content)),
                b'0' | 0 => {
                    let k = key(&Path::new(root).join(&name));
                    if !only_remote || matches!(vfs.nodes.get(&k), Some(Node::Remote { .. })) {
                        vfs.put(
                            k,
                            Node::File {
                                data: content.to_vec(),
                                modified: SystemTime::now(),
                            },
                        );
                        count += 1;
                    }
                }
                b'5' => vfs.mkdirs(&key(&Path::new(root).join(&name))),
                _ => {}
            }
            at = body + size.div_ceil(512) * 512;
        }
        Ok(count)
    }

    /// Lists files under `root` to fetch from `base` when first read: one
    /// path per line, relative to `root`, then a tab and its size, and
    /// optionally a tab and the pack that holds it (relative to `base`).
    /// Files already present are kept.
    pub fn install_manifest(root: &str, base: &str, manifest: &str) {
        *crate::sync::lock(&REMOTE).unwrap_or_else(|e| e.into_inner()) =
            (key(Path::new(root)), base.to_owned());
        let mut vfs = vfs();
        let mut packs: Vec<Arc<str>> = Vec::new();
        for line in manifest.lines().filter(|l| !l.is_empty()) {
            let mut fields = line.split('\t');
            let path = fields.next().unwrap_or(line);
            let len = fields.next().and_then(|l| l.parse().ok()).unwrap_or(0);
            // One shared name per pack.
            let pack = fields
                .next()
                .map(|p| match packs.iter().find(|q| &***q == p) {
                    Some(q) => q.clone(),
                    None => {
                        packs.push(p.into());
                        packs[packs.len() - 1].clone()
                    }
                });
            let k = key(&Path::new(root).join(path));
            if !vfs.nodes.contains_key(&k) {
                vfs.put(k, Node::Remote { len, pack });
            }
        }
    }

    /// A file the page saved in the browser's storage, back in place.
    pub fn install_saved_file(path: &str, data: Vec<u8>, modified_ms: f64) {
        let modified = SystemTime::UNIX_EPOCH + Duration::from_millis(modified_ms.max(0.0) as u64);
        vfs().put(key(Path::new(path)), Node::File { data, modified });
    }

    /// A folder the page saved in the browser's storage, back in place.
    pub fn install_saved_dir(path: &str) {
        vfs().mkdirs(&key(Path::new(path)));
    }

    /// What changed since the last call, parents before children: each path
    /// with its state now.
    pub fn take_changes() -> Vec<(String, Change)> {
        let mut vfs = vfs();
        let dirty = std::mem::take(&mut vfs.dirty);
        dirty
            .into_iter()
            .map(|(key, clear)| {
                let change = match vfs.nodes.get(&key) {
                    Some(Node::File { data, .. }) => Change::File(data.clone()),
                    Some(Node::Dir) => Change::Dir { clear },
                    Some(Node::Remote { .. }) | None => Change::Gone,
                };
                (key, change)
            })
            .collect()
    }

    pub fn exists(path: impl AsRef<Path>) -> bool {
        vfs().nodes.contains_key(&key(path.as_ref()))
    }

    pub fn is_dir(path: impl AsRef<Path>) -> bool {
        matches!(vfs().nodes.get(&key(path.as_ref())), Some(Node::Dir))
    }

    pub fn is_file(path: impl AsRef<Path>) -> bool {
        matches!(
            vfs().nodes.get(&key(path.as_ref())),
            Some(Node::File { .. } | Node::Remote { .. })
        )
    }

    pub fn read(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
        let path = path.as_ref();
        let k = key(path);
        prefetch(&k);
        vfs().data(&k, path).cloned()
    }

    pub fn read_to_string(path: impl AsRef<Path>) -> io::Result<String> {
        String::from_utf8(read(path)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    pub fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
        let path = path.as_ref();
        let k = key(path);
        let mut vfs = vfs();
        match parent_key(&k).and_then(|p| vfs.nodes.get(p)) {
            Some(Node::Dir) => {}
            _ => return Err(not_found(path)),
        }
        if matches!(vfs.nodes.get(&k), Some(Node::Dir)) {
            return Err(io::Error::new(
                io::ErrorKind::IsADirectory,
                "is a directory",
            ));
        }
        vfs.touch(&k, false);
        vfs.nodes.insert(
            k,
            Node::File {
                data: contents.as_ref().to_vec(),
                modified: SystemTime::now(),
            },
        );
        Ok(())
    }

    pub fn create_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
        let k = key(path.as_ref());
        let mut vfs = vfs();
        if matches!(
            vfs.nodes.get(&k),
            Some(Node::File { .. } | Node::Remote { .. })
        ) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "a file is in the way",
            ));
        }
        vfs.mkdirs(&k);
        vfs.touch(&k, false);
        Ok(())
    }

    pub fn create_dir(path: impl AsRef<Path>) -> io::Result<()> {
        let path = path.as_ref();
        let k = key(path);
        let mut vfs = vfs();
        if vfs.nodes.contains_key(&k) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "already exists",
            ));
        }
        match parent_key(&k).and_then(|p| vfs.nodes.get(p)) {
            Some(Node::Dir) => {
                vfs.touch(&k, false);
                vfs.nodes.insert(k, Node::Dir);
                Ok(())
            }
            _ => Err(not_found(path)),
        }
    }

    pub fn remove_file(path: impl AsRef<Path>) -> io::Result<()> {
        let path = path.as_ref();
        let k = key(path);
        let mut vfs = vfs();
        match vfs.nodes.get(&k) {
            Some(Node::File { .. } | Node::Remote { .. }) => {
                vfs.nodes.remove(&k);
                vfs.touch(&k, false);
                Ok(())
            }
            Some(Node::Dir) => Err(io::Error::new(
                io::ErrorKind::IsADirectory,
                "is a directory",
            )),
            None => Err(not_found(path)),
        }
    }

    pub fn remove_dir(path: impl AsRef<Path>) -> io::Result<()> {
        let path = path.as_ref();
        let k = key(path);
        let mut vfs = vfs();
        if !matches!(vfs.nodes.get(&k), Some(Node::Dir)) {
            return Err(not_found(path));
        }
        if !vfs.children(&k).is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::DirectoryNotEmpty,
                "not empty",
            ));
        }
        vfs.nodes.remove(&k);
        vfs.touch(&k, false);
        Ok(())
    }

    pub fn remove_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
        let path = path.as_ref();
        let k = key(path);
        let mut vfs = vfs();
        if !vfs.nodes.contains_key(&k) {
            return Err(not_found(path));
        }
        let prefix = format!("{k}/");
        vfs.nodes.retain(|n, _| n != &k && !n.starts_with(&prefix));
        vfs.touch(&k, false);
        Ok(())
    }

    pub fn copy(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<u64> {
        let data = read(from)?;
        let len = data.len() as u64;
        write(to, data)?;
        Ok(len)
    }

    pub fn rename(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
        let (from, to) = (from.as_ref(), to.as_ref());
        let (fk, tk) = (key(from), key(to));
        let mut vfs = vfs();
        if !vfs.nodes.contains_key(&fk) {
            return Err(not_found(from));
        }
        let prefix = format!("{fk}/");
        let moved: Vec<(String, Node)> = vfs
            .nodes
            .iter()
            .filter(|(k, _)| **k == fk || k.starts_with(&prefix))
            .map(|(k, n)| (format!("{tk}{}", &k[fk.len()..]), n.clone()))
            .collect();
        vfs.nodes.retain(|k, _| *k != fk && !k.starts_with(&prefix));
        let tprefix = format!("{tk}/");
        vfs.nodes
            .retain(|k, _| *k != tk && !k.starts_with(&tprefix));
        vfs.touch(&fk, false);
        vfs.touch(&tk, true);
        for (k, n) in moved {
            vfs.touch(&k, false);
            vfs.put(k, n);
        }
        Ok(())
    }

    pub fn canonicalize(path: impl AsRef<Path>) -> io::Result<PathBuf> {
        let path = path.as_ref();
        let k = key(path);
        if vfs().nodes.contains_key(&k) {
            Ok(PathBuf::from(k))
        } else {
            Err(not_found(path))
        }
    }

    pub fn read_link(path: impl AsRef<Path>) -> io::Result<PathBuf> {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{}: not a symbolic link", path.as_ref().display()),
        ))
    }

    /// No links in memory: a copy stands in.
    pub fn hard_link(original: impl AsRef<Path>, link: impl AsRef<Path>) -> io::Result<()> {
        copy(original, link).map(|_| ())
    }

    pub fn metadata(path: impl AsRef<Path>) -> io::Result<Metadata> {
        let path = path.as_ref();
        match vfs().nodes.get(&key(path)) {
            None => Err(not_found(path)),
            Some(Node::Dir) => Ok(Metadata {
                dir: true,
                len: 0,
                modified: SystemTime::UNIX_EPOCH,
            }),
            Some(Node::File { data, modified }) => Ok(Metadata {
                dir: false,
                len: data.len() as u64,
                modified: *modified,
            }),
            Some(Node::Remote { len, .. }) => Ok(Metadata {
                dir: false,
                len: *len,
                modified: SystemTime::UNIX_EPOCH,
            }),
        }
    }

    pub fn symlink_metadata(path: impl AsRef<Path>) -> io::Result<Metadata> {
        metadata(path)
    }

    pub fn set_permissions(_path: impl AsRef<Path>, _perm: Permissions) -> io::Result<()> {
        Ok(())
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Permissions {
        readonly: bool,
    }
    impl Permissions {
        pub fn readonly(&self) -> bool {
            self.readonly
        }
        pub fn set_readonly(&mut self, readonly: bool) {
            self.readonly = readonly;
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct FileType {
        dir: bool,
    }
    impl FileType {
        pub fn is_dir(&self) -> bool {
            self.dir
        }
        pub fn is_file(&self) -> bool {
            !self.dir
        }
        pub fn is_symlink(&self) -> bool {
            false
        }
    }

    #[derive(Clone, Debug)]
    pub struct Metadata {
        dir: bool,
        len: u64,
        modified: SystemTime,
    }
    // std's Metadata has `len` without `is_empty` too.
    #[allow(clippy::len_without_is_empty)]
    impl Metadata {
        pub fn is_dir(&self) -> bool {
            self.dir
        }
        pub fn is_file(&self) -> bool {
            !self.dir
        }
        pub fn is_symlink(&self) -> bool {
            false
        }
        pub fn len(&self) -> u64 {
            self.len
        }
        pub fn file_type(&self) -> FileType {
            FileType { dir: self.dir }
        }
        pub fn modified(&self) -> io::Result<SystemTime> {
            Ok(self.modified)
        }
        pub fn accessed(&self) -> io::Result<SystemTime> {
            Ok(self.modified)
        }
        pub fn created(&self) -> io::Result<SystemTime> {
            Ok(self.modified)
        }
        pub fn permissions(&self) -> Permissions {
            Permissions { readonly: false }
        }
    }

    #[derive(Debug)]
    pub struct DirEntry {
        path: PathBuf,
    }
    impl DirEntry {
        pub fn path(&self) -> PathBuf {
            self.path.clone()
        }
        pub fn file_name(&self) -> OsString {
            self.path.file_name().unwrap_or_default().to_owned()
        }
        pub fn metadata(&self) -> io::Result<Metadata> {
            metadata(&self.path)
        }
        pub fn file_type(&self) -> io::Result<FileType> {
            metadata(&self.path).map(|m| m.file_type())
        }
    }

    #[derive(Debug)]
    pub struct ReadDir(std::vec::IntoIter<DirEntry>);
    impl Iterator for ReadDir {
        type Item = io::Result<DirEntry>;
        fn next(&mut self) -> Option<Self::Item> {
            self.0.next().map(Ok)
        }
    }

    /// The entries of `path`, joined to `path` as given (as std does), in
    /// name order.
    pub fn read_dir(path: impl AsRef<Path>) -> io::Result<ReadDir> {
        let path = path.as_ref();
        let k = key(path);
        let vfs = vfs();
        if !matches!(vfs.nodes.get(&k), Some(Node::Dir)) {
            return Err(not_found(path));
        }
        let entries = vfs
            .children(&k)
            .into_iter()
            .map(|child| DirEntry {
                path: path.join(&child[child.rfind('/').map_or(0, |i| i + 1)..]),
            })
            .collect::<Vec<_>>();
        Ok(ReadDir(entries.into_iter()))
    }

    #[derive(Clone, Debug, Default)]
    pub struct FileTimes {
        modified: Option<SystemTime>,
    }
    impl FileTimes {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn set_modified(mut self, t: SystemTime) -> Self {
            self.modified = Some(t);
            self
        }
        pub fn set_accessed(self, _t: SystemTime) -> Self {
            self
        }
    }

    #[derive(Debug)]
    pub enum TryLockError {
        Error(io::Error),
        WouldBlock,
    }
    impl std::fmt::Display for TryLockError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                TryLockError::Error(e) => e.fmt(f),
                TryLockError::WouldBlock => f.write_str("lock would block"),
            }
        }
    }
    impl std::error::Error for TryLockError {}

    #[derive(Clone, Debug, Default)]
    pub struct OpenOptions {
        read: bool,
        write: bool,
        append: bool,
        truncate: bool,
        create: bool,
        create_new: bool,
    }
    impl OpenOptions {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn read(&mut self, v: bool) -> &mut Self {
            self.read = v;
            self
        }
        pub fn write(&mut self, v: bool) -> &mut Self {
            self.write = v;
            self
        }
        pub fn append(&mut self, v: bool) -> &mut Self {
            self.append = v;
            self
        }
        pub fn truncate(&mut self, v: bool) -> &mut Self {
            self.truncate = v;
            self
        }
        pub fn create(&mut self, v: bool) -> &mut Self {
            self.create = v;
            self
        }
        pub fn create_new(&mut self, v: bool) -> &mut Self {
            self.create_new = v;
            self
        }
        pub fn open(&self, path: impl AsRef<Path>) -> io::Result<File> {
            let path = path.as_ref();
            let k = key(path);
            prefetch(&k);
            let mut vfs = vfs();
            let present = vfs.nodes.contains_key(&k);
            if self.create_new && present {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "already exists",
                ));
            }
            if !present {
                if !(self.create || self.create_new) {
                    return Err(not_found(path));
                }
                match parent_key(&k).and_then(|p| vfs.nodes.get(p)) {
                    Some(Node::Dir) => {}
                    _ => return Err(not_found(path)),
                }
                vfs.nodes.insert(
                    k.clone(),
                    Node::File {
                        data: Vec::new(),
                        modified: SystemTime::now(),
                    },
                );
            }
            if !present || (self.truncate && (self.write || self.append)) {
                vfs.touch(&k, false);
            }
            let data = vfs.data(&k, path)?;
            if self.truncate && (self.write || self.append) {
                data.clear();
            }
            let pos = if self.append { data.len() as u64 } else { 0 };
            Ok(File {
                key: k,
                path: path.to_path_buf(),
                pos,
                write: self.write || self.append,
                append: self.append,
            })
        }
    }

    /// An open file: reads and writes go straight to the file's bytes in the
    /// virtual filesystem.
    #[derive(Debug)]
    pub struct File {
        key: String,
        path: PathBuf,
        pos: u64,
        write: bool,
        append: bool,
    }
    impl File {
        pub fn open(path: impl AsRef<Path>) -> io::Result<File> {
            OpenOptions::new().read(true).open(path)
        }
        pub fn create(path: impl AsRef<Path>) -> io::Result<File> {
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
        }
        pub fn create_new(path: impl AsRef<Path>) -> io::Result<File> {
            OpenOptions::new().write(true).create_new(true).open(path)
        }
        pub fn options() -> OpenOptions {
            OpenOptions::new()
        }
        pub fn metadata(&self) -> io::Result<Metadata> {
            metadata(&self.path)
        }
        pub fn set_len(&self, len: u64) -> io::Result<()> {
            let mut vfs = vfs();
            vfs.touch(&self.key, false);
            vfs.data(&self.key, &self.path)?.resize(len as usize, 0);
            Ok(())
        }
        pub fn sync_all(&self) -> io::Result<()> {
            Ok(())
        }
        pub fn sync_data(&self) -> io::Result<()> {
            Ok(())
        }
        pub fn set_times(&self, times: FileTimes) -> io::Result<()> {
            if let Some(t) = times.modified {
                self.set_modified(t)?;
            }
            Ok(())
        }
        pub fn set_modified(&self, t: SystemTime) -> io::Result<()> {
            if let Some(Node::File { modified, .. }) = vfs().nodes.get_mut(&self.key) {
                *modified = t;
            }
            Ok(())
        }
        pub fn set_permissions(&self, _perm: Permissions) -> io::Result<()> {
            Ok(())
        }
        /// One page, one game: nothing else holds the lock.
        pub fn lock(&self) -> io::Result<()> {
            Ok(())
        }
        pub fn lock_shared(&self) -> io::Result<()> {
            Ok(())
        }
        pub fn try_lock(&self) -> Result<(), TryLockError> {
            Ok(())
        }
        pub fn try_lock_shared(&self) -> Result<(), TryLockError> {
            Ok(())
        }
        pub fn unlock(&self) -> io::Result<()> {
            Ok(())
        }
        pub fn try_clone(&self) -> io::Result<File> {
            Ok(File {
                key: self.key.clone(),
                path: self.path.clone(),
                pos: self.pos,
                write: self.write,
                append: self.append,
            })
        }
    }
    impl Read for File {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let mut vfs = vfs();
            let data = vfs.data(&self.key, &self.path)?;
            let start = (self.pos as usize).min(data.len());
            let n = buf.len().min(data.len() - start);
            buf[..n].copy_from_slice(&data[start..start + n]);
            self.pos += n as u64;
            Ok(n)
        }
    }
    impl Read for &File {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            // A shared reference has no cursor of its own: read from the start.
            let mut vfs = vfs();
            let data = vfs.data(&self.key, &self.path)?;
            let n = buf.len().min(data.len());
            buf[..n].copy_from_slice(&data[..n]);
            Ok(n)
        }
    }
    impl Write for File {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if !self.write {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "not open for writing",
                ));
            }
            let mut vfs = vfs();
            vfs.touch(&self.key, false);
            let data = vfs.data(&self.key, &self.path)?;
            if self.append {
                self.pos = data.len() as u64;
            }
            let start = self.pos as usize;
            if data.len() < start + buf.len() {
                data.resize(start + buf.len(), 0);
            }
            data[start..start + buf.len()].copy_from_slice(buf);
            self.pos += buf.len() as u64;
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl Seek for File {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            let len = {
                let mut vfs = vfs();
                vfs.data(&self.key, &self.path)?.len() as i64
            };
            let next = match pos {
                SeekFrom::Start(p) => p as i64,
                SeekFrom::End(d) => len + d,
                SeekFrom::Current(d) => self.pos as i64 + d,
            };
            if next < 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "seek before start",
                ));
            }
            self.pos = next as u64;
            Ok(self.pos)
        }
    }
}
