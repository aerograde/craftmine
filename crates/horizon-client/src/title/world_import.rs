//! Importing a world folder from a ZIP archive (vanilla's backup layout and
//! the world exports other launchers make) or an Eaglercraft `.epk`. The
//! archive must hold a `level.dat`; its folder becomes a new save under
//! `saves/`. Nothing is upgraded here: the open flow runs the file fixes and
//! the datafixer on the first open, exactly as it does for a world made
//! elsewhere.
use crate::Result;
use horizon_core::position::ChunkPos;
use horizon_os::fs::{self, PathExt};
use horizon_world::{
    compression::Compression,
    nbt::{self, Limits, NamedTag, Tag},
    region::{ChunkRecord, RegionFile},
};
use sha1::{Digest, Sha1};
use std::{
    io::{Cursor, Read, Seek, SeekFrom},
    path::{Component, Path},
};

/// The most an import inflates to: far above a real save, a zip bomb stops
/// here (the browser build would run out of memory long before).
const MAX_BYTES: u64 = 4 << 30;
/// The most one entry may be: far above a region file, and small enough to
/// allocate in the browser build's address space.
const MAX_ENTRY: u64 = 1 << 30;
/// The most entries an archive may hold: a world has thousands, not millions.
const MAX_ENTRIES: usize = 200_000;
/// The two Eaglercraft EPK headers: `EPKDecompilerSP` (eagler-binary-tools).
const EPK_NEW: &[u8; 8] = b"EAGPKG$$";
const EPK_OLD: &[u8; 8] = b"EAGPKG!!";
const EPK_END: &[u8; 8] = b":::YEE:>";

/// Imports the world read from `reader`, whichever container it is in, as a
/// folder named after `name` (a file name or a player's title). A ZIP is
/// streamed; an EPK is read whole, as its files are compressed inside it.
pub fn import<R: Read + Seek>(mut reader: R, name: &str, saves: &Path) -> Result<String> {
    let mut magic = [0u8; 8];
    let mut filled = 0;
    while filled < magic.len() {
        match reader.read(&mut magic[filled..])? {
            0 => break,
            read => filled += read,
        }
    }
    reader.seek(SeekFrom::Start(0))?;
    if filled == 8 && (&magic == EPK_NEW || &magic == EPK_OLD) {
        let mut bytes = Vec::new();
        reader.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("The archive is too large to import.".into());
        }
        return import_files(epk_files(&bytes)?, name, saves);
    }
    import_zip(reader, name, saves)
}

/// Imports the ZIP archive read from `reader` into `saves`.
fn import_zip<R: Read + Seek>(reader: R, name: &str, saves: &Path) -> Result<String> {
    let mut archive =
        zip::ZipArchive::new(reader).map_err(|e| format!("Not a world ZIP archive: {e}"))?;
    if archive.len() > MAX_ENTRIES {
        return Err("The archive holds too many files to import.".into());
    }
    let root = zip_world_root(&mut archive)?;
    let folder = unique_folder(saves, &folder_name(name))?;
    let target = saves.join(&folder);
    fs::create_dir_all(&target)?;
    let result = write_zip(&mut archive, &root, &target);
    if let Err(e) = result {
        // A half-imported world would show up in the list: take it away.
        let _ = fs::remove_dir_all(&target);
        return Err(e);
    }
    Ok(folder)
}

/// Writes an EPK's files (already unpacked and checked) as a new save.
fn import_files(files: Vec<(String, Vec<u8>)>, name: &str, saves: &Path) -> Result<String> {
    let root = world_root_of(files.iter().map(|(name, _)| name.as_str()))?;
    let folder = unique_folder(saves, &folder_name(name))?;
    let target = saves.join(&folder);
    fs::create_dir_all(&target)?;
    let result = write_files(&files, &root, &target).and_then(|()| convert_eagler_chunks(&target));
    if let Err(e) = result {
        let _ = fs::remove_dir_all(&target);
        return Err(e);
    }
    Ok(folder)
}

/// Eaglercraft 1.8 saves a chunk as a gzip NBT file under `level0`,
/// `level-1` or `level1`, while Horizon reads Anvil `region/` files. This is
/// the inverse of the exporter's `WorldConverterMCA.exportWorld`: group the
/// chunks by region and write them as `.mca` (zlib chunk payloads).
fn convert_eagler_chunks(target: &Path) -> Result<()> {
    for (source, destination) in [
        ("level0", "region"),
        ("level-1", "DIM-1/region"),
        ("level1", "DIM1/region"),
    ] {
        let source = target.join(source);
        if !source.fs_is_dir() {
            continue;
        }
        let mut regions: std::collections::BTreeMap<(i32, i32), Vec<(i32, i32, NamedTag)>> =
            Default::default();
        for entry in fs::read_dir(&source)? {
            let path = entry?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("dat") {
                continue;
            }
            let bytes = fs::read(&path)?;
            // Eaglercraft pads a chunk file after its gzip member; decode the
            // member alone rather than letting a multi-member decoder read the
            // padding as another header.
            let mut raw = Vec::new();
            flate2::read::GzDecoder::new(bytes.as_slice())
                .take(Limits::saved().max_bytes as u64 + 1)
                .read_to_end(&mut raw)?;
            let (named, _) = nbt::read_named(&raw, Limits::saved())?;
            let level = named.tag.get("Level").unwrap_or(&named.tag);
            let (Some(Tag::Int(x)), Some(Tag::Int(z))) = (level.get("xPos"), level.get("zPos"))
            else {
                return Err(
                    format!("Eaglercraft chunk {} has no xPos/zPos.", path.display()).into(),
                );
            };
            regions
                .entry((*x >> 5, *z >> 5))
                .or_default()
                .push((*x, *z, named));
        }
        for ((region_x, region_z), chunks) in regions {
            let directory = target.join(destination);
            fs::create_dir_all(&directory)?;
            let path = directory.join(format!("r.{region_x}.{region_z}.mca"));
            let mut region = RegionFile::open_writable(&path, true)?;
            for (x, z, named) in chunks {
                let record = ChunkRecord::from_nbt(&named, Compression::Zlib, 0, Limits::saved())?;
                region.write(ChunkPos::new(x, z), &record)?;
            }
        }
        fs::remove_dir_all(&source)?;
    }
    Ok(())
}

/// The folder the archive's world starts at: the parent of its `level.dat`,
/// `""` when the files are at the archive's root. The shallowest one wins; two
/// at the same depth would be two worlds.
fn world_root_of<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<String> {
    let mut root: Option<String> = None;
    for name in names {
        let Some(parent) = name
            .strip_suffix("/level.dat")
            .or((name == "level.dat").then_some(""))
        else {
            continue;
        };
        match &root {
            None => root = Some(parent.to_owned()),
            Some(world) if world.as_str() == parent => {}
            Some(world) if world_depth(world) > world_depth(parent) => {
                root = Some(parent.to_owned())
            }
            Some(world) if world_depth(world) == world_depth(parent) => {
                return Err("The archive holds more than one world.".into());
            }
            Some(_) => {}
        }
    }
    root.ok_or_else(|| "The archive has no level.dat: not a world.".into())
}

fn world_depth(root: &str) -> usize {
    if root.is_empty() {
        0
    } else {
        root.split('/').count()
    }
}

fn zip_world_root<R: Read + Seek>(archive: &mut zip::ZipArchive<R>) -> Result<String> {
    let mut names = Vec::new();
    for i in 0..archive.len() {
        let entry = archive.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let entry_name = entry.name()?;
        names.push(entry_path(&entry_name)?);
    }
    world_root_of(names.iter().map(String::as_str))
}

/// Copies the ZIP entries under `root` into `target`, keeping their relative
/// paths. The session lock and files outside the world (a macOS `__MACOSX`
/// folder, say) are left behind, as a backup never has them.
fn write_zip<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    root: &str,
    target: &Path,
) -> Result<()> {
    let prefix = root_prefix(root);
    let mut total = 0u64;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let entry_name = entry.name()?;
        let name = entry_path(&entry_name)?;
        let relative = name.strip_prefix(&prefix).unwrap_or("");
        if !keep_entry(relative)
            || (entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000))
        {
            // A lock would refuse the world it came from, and a link is never
            // unpacked.
            continue;
        }
        let size = entry.size();
        total = total
            .checked_add(size)
            .filter(|&total| total <= MAX_BYTES)
            .ok_or("The archive is too large to import.")?;
        write_file(target, relative, &mut entry, size)?;
    }
    Ok(())
}

/// Writes an EPK's files, keeping the same rules as `write_zip`.
fn write_files(files: &[(String, Vec<u8>)], root: &str, target: &Path) -> Result<()> {
    let prefix = root_prefix(root);
    let mut total = 0u64;
    for (name, data) in files {
        let relative = name.strip_prefix(&prefix).unwrap_or("");
        if !keep_entry(relative) {
            continue;
        }
        total = total
            .checked_add(data.len() as u64)
            .filter(|&total| total <= MAX_BYTES)
            .ok_or("The archive is too large to import.")?;
        write_file(target, relative, &mut data.as_slice(), data.len() as u64)?;
    }
    Ok(())
}

fn root_prefix(root: &str) -> String {
    if root.is_empty() {
        String::new()
    } else {
        format!("{root}/")
    }
}

fn keep_entry(relative: &str) -> bool {
    !relative.is_empty() && relative != "session.lock"
}

/// Writes one file of a world, refusing more bytes than the archive declared.
fn write_file(target: &Path, relative: &str, data: &mut impl Read, size: u64) -> Result<()> {
    let path = target.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut out = fs::File::create(&path)?;
    let copied = std::io::copy(&mut data.take(size + 1), &mut out)?;
    if copied > size {
        return Err(format!("{relative} is larger than the archive says.").into());
    }
    Ok(())
}

/// The entry's name as a relative path with `/` separators (Windows
/// separators read as `/`, a `./` prefix dropped). An absolute path, a
/// `..` or an empty name is refused, so nothing is written outside the save.
fn entry_path(name: &str) -> Result<String> {
    let name = name.replace('\\', "/");
    let name = name.trim_start_matches("./");
    let path = Path::new(name);
    if name.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("The archive holds an unsafe path: {name}").into());
    }
    Ok(name.to_owned())
}

/// A folder name for the world: the file name without its extension, with
/// the characters file systems refuse (and control characters) replaced.
fn folder_name(name: &str) -> String {
    let mut name = name.trim();
    for extension in [".zip", ".epk"] {
        if name.len() >= extension.len()
            && name[name.len() - extension.len()..].eq_ignore_ascii_case(extension)
        {
            name = &name[..name.len() - extension.len()];
        }
    }
    let mut cleaned = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_control() || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            cleaned.push('_');
        } else {
            cleaned.push(c);
        }
    }
    // Windows refuses names ending in a dot or a space.
    let cleaned = cleaned.trim().trim_end_matches(['.', ' ']);
    if cleaned.is_empty() {
        return "Imported World".into();
    }
    cleaned.chars().take(64).collect()
}

/// `folder`, or the first free `folder (2)`, `folder (3)`... in `saves`.
fn unique_folder(saves: &Path, folder: &str) -> Result<String> {
    for n in 1..=1000 {
        let name = if n == 1 {
            folder.to_owned()
        } else {
            format!("{folder} ({n})")
        };
        if fs::symlink_metadata(saves.join(&name)).is_err() {
            return Ok(name);
        }
    }
    Err(format!("There are too many worlds named {folder}.").into())
}

/// The files of an Eaglercraft EPK, unpacked and checked. Only `FILE` entries
/// are kept (a `HEAD` holds the package's metadata); paths go through
/// `entry_path` like a ZIP's.
fn epk_files(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    if bytes.len() < 16 {
        return Err("The file is too short to be an EPK.".into());
    }
    if &bytes[..8] == EPK_NEW {
        if &bytes[bytes.len() - 8..] != EPK_END {
            return Err("The EPK is missing its end marker (:::YEE:>).".into());
        }
        epk_files_new(&bytes[8..bytes.len() - 8])
    } else if &bytes[..8] == EPK_OLD {
        epk_files_old(&bytes[8..])
    } else {
        Err("Not an EPK archive.".into())
    }
}

/// The `EAGPKG$$` format: a versioned header, then the entries in one zlib,
/// gzip or stored stream, each with its CRC32.
fn epk_files_new(body: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let mut header = Cursor::new(body);
    let version = read_ascii(&mut header)?;
    if !version.starts_with("ver2.") {
        return Err(format!("Unsupported EPK version: {version}").into());
    }
    let name_len = read_u8(&mut header)? as usize;
    header.seek(SeekFrom::Current(name_len as i64))?;
    let comment_len = read_u16(&mut header)? as usize;
    header.seek(SeekFrom::Current(comment_len as i64))?;
    header.seek(SeekFrom::Current(8))?;
    let count = read_u32(&mut header)? as usize;
    if count > MAX_ENTRIES {
        return Err("The EPK holds too many files to import.".into());
    }
    let compression = read_u8(&mut header)?;
    let at = header.position() as usize;
    if at > body.len() {
        return Err("The EPK header is truncated.".into());
    }
    let payload = &body[at..];
    let stream: Box<dyn Read> = match compression {
        b'Z' => Box::new(flate2::read::ZlibDecoder::new(payload)),
        b'G' => Box::new(flate2::read::GzDecoder::new(payload)),
        b'0' => Box::new(payload),
        other => {
            return Err(format!("Unsupported EPK compression: {}", other as char).into());
        }
    };
    let mut stream = stream.take(MAX_BYTES + 1);
    let mut files = Vec::new();
    let mut total = 0u64;
    for _ in 0..count {
        let kind = read_exact::<4, _>(&mut stream)?;
        if &kind == b"END$" {
            return Err("The EPK ends before its file count.".into());
        }
        let name = entry_path(&read_ascii(&mut stream)?)?;
        let len = read_u32(&mut stream)? as u64;
        if len > MAX_ENTRY {
            return Err(format!("EPK entry {name} is too large to import.").into());
        }
        total = total
            .checked_add(len)
            .filter(|&total| total <= MAX_BYTES)
            .ok_or("The EPK unpacks to more than an import takes.")?;
        let data = if &kind == b"FILE" {
            if len < 5 {
                return Err(format!("EPK file {name} is incomplete (no CRC).").into());
            }
            let crc = read_u32(&mut stream)?;
            let mut data = vec![0; (len - 5) as usize];
            stream.read_exact(&mut data)?;
            let mut hasher = flate2::Crc::new();
            hasher.update(&data);
            if hasher.sum() != crc {
                return Err(format!("EPK file {name} has an invalid checksum.").into());
            }
            if read_u8(&mut stream)? != b':' {
                return Err(format!("EPK file {name} is incomplete.").into());
            }
            data
        } else {
            let mut data = vec![0; len as usize];
            stream.read_exact(&mut data)?;
            data
        };
        if read_u8(&mut stream)? != b'>' {
            return Err(format!("EPK entry {name} is incomplete.").into());
        }
        // The exporter's first HEAD says which world format it packed; 1.5.2
        // worlds need a datafixer Horizon does not carry.
        if &kind == b"HEAD" && name == "file-type" && data == b"epk/world152" {
            return Err(
                "The EPK holds a Minecraft 1.5.2 world; Horizon imports 1.8 worlds.".into(),
            );
        }
        if &kind == b"FILE" {
            files.push((name, data));
        }
    }
    if &read_exact::<4, _>(&mut stream)? != b"END$" {
        return Err("The EPK is missing its END marker.".into());
    }
    Ok(files)
}

/// The legacy `EAGPKG!!` format: a UTF comment, then a zlib stream of
/// `<file>` entries, each with its SHA-1, and a final " end".
fn epk_files_old(body: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let mut header = Cursor::new(body);
    let _comment = read_utf(&mut header)?;
    let payload = &body[header.position() as usize..];
    let mut stream = flate2::read::ZlibDecoder::new(payload).take(MAX_BYTES + 1);
    let mut files = Vec::new();
    let mut total = 0u64;
    loop {
        let marker = read_utf(&mut stream)?;
        if marker == " end" {
            break;
        }
        if marker != "<file>" {
            return Err("The EPK holds an invalid entry.".into());
        }
        let name = entry_path(&read_utf(&mut stream)?)?;
        let mut digest = [0u8; 20];
        stream.read_exact(&mut digest)?;
        let len = read_u32(&mut stream)? as u64;
        if len > MAX_ENTRY {
            return Err(format!("EPK entry {name} is too large to import.").into());
        }
        total = total
            .checked_add(len)
            .filter(|&total| total <= MAX_BYTES)
            .ok_or("The EPK unpacks to more than an import takes.")?;
        let mut data = vec![0; len as usize];
        stream.read_exact(&mut data)?;
        let mut hasher = Sha1::new();
        hasher.update(&data);
        if hasher.finalize().as_slice() != digest {
            return Err(format!("EPK file {name} has an invalid hash.").into());
        }
        if read_utf(&mut stream)? != "</file>" {
            return Err(format!("EPK file {name} is incomplete.").into());
        }
        files.push((name, data));
        if files.len() > MAX_ENTRIES {
            return Err("The EPK holds too many files to import.".into());
        }
    }
    Ok(files)
}

fn read_exact<const N: usize, R: Read>(reader: &mut R) -> Result<[u8; N]> {
    let mut out = [0; N];
    reader.read_exact(&mut out)?;
    Ok(out)
}

fn read_u8(reader: &mut impl Read) -> Result<u8> {
    Ok(read_exact::<1, _>(reader)?[0])
}

fn read_u16(reader: &mut impl Read) -> Result<u16> {
    Ok(u16::from_be_bytes(read_exact(reader)?))
}

fn read_u32(reader: &mut impl Read) -> Result<u32> {
    Ok(u32::from_be_bytes(read_exact(reader)?))
}

/// An EPK string: one length byte, then that many bytes.
fn read_ascii(reader: &mut impl Read) -> Result<String> {
    let len = read_u8(reader)? as usize;
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Java's `DataInputStream.readUTF` (the length Java writes for short
/// strings; the modified-UTF corner cases do not appear in world paths).
fn read_utf(reader: &mut impl Read) -> Result<String> {
    let len = read_u16(reader)? as usize;
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Asks the browser for a world archive; the title screen's poll takes it.
/// Other builds import through the control socket (`world-import PATH`).
pub fn pick() {
    #[cfg(target_arch = "wasm32")]
    horizon_os::web::pick_file("world", ".zip,.epk,application/zip");
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_os::fs::PathExt;
    use std::{
        io::Write,
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    struct Scratch {
        root: PathBuf,
    }
    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let root = horizon_os::env::temp_dir().join(format!(
                "horizon-world-import-{}-{}",
                horizon_os::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(root.join("saves")).unwrap();
            Self { root }
        }
        fn saves(&self) -> PathBuf {
            self.root.join("saves")
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn archive(entries: &[(&str, &[u8])]) -> Cursor<Vec<u8>> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap()
    }

    fn push_ascii(out: &mut Vec<u8>, text: &str) {
        out.push(text.len() as u8);
        out.extend_from_slice(text.as_bytes());
    }

    fn push_utf(out: &mut Vec<u8>, text: &str) {
        out.extend_from_slice(&(text.len() as u16).to_be_bytes());
        out.extend_from_slice(text.as_bytes());
    }

    /// A `EAGPKG$$` archive: a `HEAD` entry, the files, one zlib stream, and
    /// the `:::YEE:>` footer, as `EPKDecompilerSP` reads it.
    fn epk_new(files: &[(&str, &[u8])]) -> Vec<u8> {
        epk_new_with_crc(files, false)
    }

    fn epk_new_with_crc(files: &[(&str, &[u8])], corrupt_crc: bool) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(EPK_NEW);
        push_ascii(&mut out, "ver2.0");
        push_ascii(&mut out, "test");
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&0u64.to_be_bytes());
        out.extend_from_slice(&((files.len() + 1) as u32).to_be_bytes());
        out.push(b'Z');
        let mut body = Vec::new();
        body.extend_from_slice(b"HEAD");
        push_ascii(&mut body, "META");
        body.extend_from_slice(&4u32.to_be_bytes());
        body.extend_from_slice(b"meta");
        body.push(b'>');
        for (name, data) in files {
            body.extend_from_slice(b"FILE");
            push_ascii(&mut body, name);
            body.extend_from_slice(&(data.len() as u32 + 5).to_be_bytes());
            let mut crc = flate2::Crc::new();
            crc.update(data);
            body.extend_from_slice(&(crc.sum() + u32::from(corrupt_crc)).to_be_bytes());
            body.extend_from_slice(data);
            body.push(b':');
            body.push(b'>');
        }
        body.extend_from_slice(b"END$");
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&body).unwrap();
        out.extend_from_slice(&encoder.finish().unwrap());
        out.extend_from_slice(EPK_END);
        out
    }

    /// The bytes `EPKCompiler` writes: three HEAD entries, stored
    /// compression, then the world files.
    fn epk_compiled(file_type: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(EPK_NEW);
        push_ascii(&mut out, "ver2.0");
        push_ascii(&mut out, "test.epk");
        push_utf(&mut out, "\n\n #  test\n\n");
        out.extend_from_slice(&0u64.to_be_bytes());
        out.extend_from_slice(&((files.len() + 3) as u32).to_be_bytes());
        out.push(b'0');
        for (name, value) in [
            ("file-type", file_type),
            ("world-name", "test"),
            ("world-owner", "owner"),
        ] {
            out.extend_from_slice(b"HEAD");
            push_ascii(&mut out, name);
            out.extend_from_slice(&(value.len() as u32).to_be_bytes());
            out.extend_from_slice(value.as_bytes());
            out.push(b'>');
        }
        for (name, data) in files {
            out.extend_from_slice(b"FILE");
            push_ascii(&mut out, name);
            out.extend_from_slice(&(data.len() as u32 + 5).to_be_bytes());
            let mut crc = flate2::Crc::new();
            crc.update(data);
            out.extend_from_slice(&crc.sum().to_be_bytes());
            out.extend_from_slice(data);
            out.push(b':');
            out.push(b'>');
        }
        out.extend_from_slice(b"END$");
        out.extend_from_slice(EPK_END);
        out
    }

    /// A legacy `EAGPKG!!` archive: a UTF comment, then the `<file>` entries
    /// with their SHA-1s in one zlib stream, ending with " end".
    fn epk_old(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(EPK_OLD);
        push_utf(&mut out, "test");
        let mut body = Vec::new();
        for (name, data) in files {
            push_utf(&mut body, "<file>");
            push_utf(&mut body, name);
            let mut hasher = Sha1::new();
            hasher.update(data);
            body.extend_from_slice(hasher.finalize().as_slice());
            body.extend_from_slice(&(data.len() as u32).to_be_bytes());
            body.extend_from_slice(data);
            push_utf(&mut body, "</file>");
        }
        push_utf(&mut body, " end");
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&body).unwrap();
        out.extend_from_slice(&encoder.finish().unwrap());
        out
    }

    #[test]
    fn imports_the_world_folder_and_leaves_the_lock_and_junk_behind() {
        let scratch = Scratch::new();
        let zip = archive(&[
            ("My World/level.dat", b"level"),
            ("My World/region/r.0.0.mca", b"chunks"),
            ("My World/players/data/player.dat", b"player"),
            ("My World/session.lock", b"lock"),
            ("__MACOSX/My World/._level.dat", b"junk"),
        ]);
        let folder = import(zip, "My World.zip", &scratch.saves()).unwrap();
        assert_eq!(folder, "My World");
        let world = scratch.saves().join(&folder);
        assert_eq!(fs::read(world.join("level.dat")).unwrap(), b"level");
        assert_eq!(fs::read(world.join("region/r.0.0.mca")).unwrap(), b"chunks");
        assert_eq!(
            fs::read(world.join("players/data/player.dat")).unwrap(),
            b"player"
        );
        assert!(!world.join("session.lock").fs_exists());
        assert!(!world.join("__MACOSX").fs_exists());
    }

    #[test]
    fn a_second_import_takes_a_new_folder() {
        let scratch = Scratch::new();
        let first = import(
            archive(&[("World/level.dat", b"one")]),
            "World",
            &scratch.saves(),
        )
        .unwrap();
        let second = import(
            archive(&[("World/level.dat", b"two")]),
            "World",
            &scratch.saves(),
        )
        .unwrap();
        assert_eq!(first, "World");
        assert_eq!(second, "World (2)");
        assert_eq!(
            fs::read(scratch.saves().join(&second).join("level.dat")).unwrap(),
            b"two"
        );
    }

    #[test]
    fn a_world_at_the_archive_root_imports_under_a_safe_name() {
        let scratch = Scratch::new();
        let zip = archive(&[("level.dat", b"level"), ("region/r.0.0.mca", b"chunks")]);
        let folder = import(zip, "..", &scratch.saves()).unwrap();
        assert_eq!(folder, "Imported World");
        assert_eq!(
            fs::read(scratch.saves().join(&folder).join("region/r.0.0.mca")).unwrap(),
            b"chunks"
        );
    }

    #[test]
    fn the_shallowest_level_is_the_world() {
        let scratch = Scratch::new();
        let zip = archive(&[
            ("world/level.dat", b"chosen"),
            ("world/region/r.0.0.mca", b"chunks"),
            ("world/old/level.dat", b"not chosen"),
        ]);
        let folder = import(zip, "Export", &scratch.saves()).unwrap();
        assert_eq!(
            fs::read(scratch.saves().join(&folder).join("level.dat")).unwrap(),
            b"chosen"
        );
    }

    #[test]
    fn refuses_an_archive_without_a_level_or_with_two() {
        let scratch = Scratch::new();
        let no_level = import(
            archive(&[("world/region/r.0.0.mca", b"chunks")]),
            "Broken",
            &scratch.saves(),
        );
        assert!(no_level.unwrap_err().to_string().contains("level.dat"));
        let two = archive(&[("a/level.dat", b"a"), ("b/level.dat", b"b")]);
        let two = import(two, "Two", &scratch.saves());
        assert!(two.unwrap_err().to_string().contains("more than one world"));
        // Nothing was left behind.
        assert!(!scratch.saves().join("Broken").fs_exists());
        assert!(!scratch.saves().join("Two").fs_exists());
    }

    #[test]
    fn imports_a_new_epk_world() {
        let scratch = Scratch::new();
        let epk = epk_new(&[
            ("world/level.dat", b"level"),
            ("world/region/r.0.0.mca", b"chunks"),
        ]);
        let folder = import(Cursor::new(epk), "Eagler World.epk", &scratch.saves()).unwrap();
        assert_eq!(folder, "Eagler World");
        let world = scratch.saves().join(&folder);
        assert_eq!(fs::read(world.join("level.dat")).unwrap(), b"level");
        assert_eq!(fs::read(world.join("region/r.0.0.mca")).unwrap(), b"chunks");
    }

    fn chunk_nbt(x: i32, z: i32) -> Vec<u8> {
        let level = Tag::compound([("xPos", Tag::Int(x)), ("zPos", Tag::Int(z))]);
        let root = NamedTag {
            name: "".into(),
            tag: Tag::compound([("Level", level)]),
        };
        let bytes = nbt::write_named(&root, Limits::saved()).unwrap();
        Compression::Gzip.encode(&bytes).unwrap()
    }

    #[test]
    fn converts_eaglercraft_chunks_to_anvil() {
        let scratch = Scratch::new();
        let chunk = chunk_nbt(2, 3);
        let epk = epk_new(&[
            ("world/level.dat", b"level"),
            ("world/level0/chunk.dat", chunk.as_slice()),
        ]);
        let folder = import(Cursor::new(epk), "Converted", &scratch.saves()).unwrap();
        let world = scratch.saves().join(&folder);
        assert!(!world.join("level0").fs_exists());
        let path = world.join("region/r.0.0.mca");
        assert!(path.fs_exists());
        let mut region = RegionFile::open(path).unwrap();
        let record = region.read(ChunkPos::new(2, 3)).unwrap().unwrap();
        let named = record.nbt(Limits::saved()).unwrap();
        assert_eq!(
            named.tag.get("Level").and_then(|level| level.get("xPos")),
            Some(&Tag::Int(2))
        );
    }

    /// A real export from a public Eaglercraft world, for the byte layout the
    /// synthetic tests cannot prove. Point `HORIZON_EPK_FIXTURE` at it.
    #[test]
    #[ignore = "set HORIZON_EPK_FIXTURE to a real .epk export"]
    fn imports_a_real_exported_epk() {
        let path = std::env::var_os("HORIZON_EPK_FIXTURE").expect("HORIZON_EPK_FIXTURE");
        let bytes = fs::read(path).unwrap();
        let scratch = Scratch::new();
        let folder = import(Cursor::new(bytes), "Public World.epk", &scratch.saves()).unwrap();
        let world = scratch.saves().join(&folder);
        assert!(world.join("level.dat").fs_exists());
        assert!(world.join("region").fs_exists());
    }

    #[test]
    fn imports_a_legacy_epk_world() {
        let scratch = Scratch::new();
        let epk = epk_old(&[
            ("world/level.dat", b"level"),
            ("world/region/r.0.0.mca", b"chunks"),
        ]);
        let folder = import(Cursor::new(epk), "Old", &scratch.saves()).unwrap();
        assert_eq!(
            fs::read(scratch.saves().join(&folder).join("level.dat")).unwrap(),
            b"level"
        );
    }

    #[test]
    fn imports_the_epk_layout_eaglercraft_actually_writes() {
        let scratch = Scratch::new();
        let epk = epk_compiled(
            "epk/world188",
            &[
                ("level.dat", b"level"),
                ("region/r.0.0.mca", b"chunks"),
                ("playerdata/one.dat", b"player"),
            ],
        );
        let folder = import(Cursor::new(epk), "Real.epk", &scratch.saves()).unwrap();
        let world = scratch.saves().join(&folder);
        assert_eq!(fs::read(world.join("level.dat")).unwrap(), b"level");
        assert_eq!(fs::read(world.join("region/r.0.0.mca")).unwrap(), b"chunks");
        assert_eq!(
            fs::read(world.join("playerdata/one.dat")).unwrap(),
            b"player"
        );
    }

    #[test]
    fn refuses_a_1_5_2_epk_with_a_clear_message() {
        let scratch = Scratch::new();
        let epk = epk_compiled("epk/world152", &[("level.dat", b"level")]);
        let error = import(Cursor::new(epk), "Old.epk", &scratch.saves());
        assert!(error.unwrap_err().to_string().contains("1.5.2"));
        assert!(!scratch.saves().join("Old").fs_exists());
    }

    #[test]
    fn refuses_an_epk_with_a_broken_checksum() {
        let scratch = Scratch::new();
        let epk = epk_new_with_crc(&[("world/level.dat", b"level")], true);
        let error = import(Cursor::new(epk), "Broken", &scratch.saves());
        assert!(error.unwrap_err().to_string().contains("checksum"));
        assert!(!scratch.saves().join("Broken").fs_exists());
    }

    #[test]
    fn entry_paths_stay_inside_the_save() {
        assert_eq!(entry_path("./world/level.dat").unwrap(), "world/level.dat");
        assert_eq!(
            entry_path("world\\region\\r.0.0.mca").unwrap(),
            "world/region/r.0.0.mca"
        );
        for unsafe_name in ["../evil/level.dat", "/absolute/level.dat", "", "."] {
            assert!(entry_path(unsafe_name).is_err(), "{unsafe_name}");
        }
    }

    #[test]
    fn folder_names_drop_extensions_and_unsafe_characters() {
        assert_eq!(folder_name("My World.zip"), "My World");
        assert_eq!(folder_name("a/b:c?.EPK"), "a_b_c_");
        assert_eq!(folder_name(" ... "), "Imported World");
        assert_eq!(folder_name(""), "Imported World");
    }
}
