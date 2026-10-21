//! Mappable sections of a v9 `.kgl` container: the section table, the aligned
//! writer plan, and the reader handles.
//!
//! A v9 container is a v8 container followed by a **data region** of raw
//! (uncompressed) sections, each starting on a 64-byte boundary so a consumer
//! can reinterpret a mapped section as a typed slice without copying. The
//! table of those sections is `FileMetadata::mappable`; offsets in it are
//! relative to the data-region base, which is `align_up(end of the compressed
//! sections, 64)`. Relative offsets avoid the circularity of absolute ones
//! (the metadata length would depend on offsets that depend on it).
//!
//! This module owns the container mechanics only. What a section *contains*
//! (embedding data, column blobs) and when a writer chooses to emit one belong
//! to the consumers; they register payloads through [`MappableSection`] and
//! read them back through [`MappedSection`].
//!
//! Integrity: the CRC32 recorded per section is checked on demand
//! ([`MappedSection::verify`]); a section carrying [`FLAG_UTF8_VALIDATED`]
//! (string payloads, which readers use without re-validating) is also checked
//! at open, and [`Verify::Eager`] checks every section at open.
use memmap2::Mmap;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

use super::load_options::{MapPolicy, Verify};
use super::{invalid_data, section_digest};

/// Alignment, in bytes, of every mappable section within the file.
pub(crate) const SECTION_ALIGNMENT: u64 = 64;

/// Entry flag: the payload is little-endian. Always set by the writer; a
/// big-endian host must decode to the heap instead of reinterpreting.
pub(crate) const FLAG_LITTLE_ENDIAN: u32 = 1;
/// Entry flag: the payload is UTF-8 that the writer validated. Such sections
/// are CRC-checked at open, because readers use the bytes without re-checking.
pub(crate) const FLAG_UTF8_VALIDATED: u32 = 1 << 1;
/// Entry flag reserved for sorted-unique slot lists; accepted, not interpreted.
pub(crate) const FLAG_SORTED_UNIQUE_SLOTS: u32 = 1 << 2;
const KNOWN_FLAGS: u32 = FLAG_LITTLE_ENDIAN | FLAG_UTF8_VALIDATED | FLAG_SORTED_UNIQUE_SLOTS;

/// Environment kill switch: `0`/`false`/`no`/`off` forces every mappable
/// section onto the heap.
pub(crate) const MMAP_ENV_VAR: &str = "KGLITE_MMAP";

/// Element type of a section; the on-disk tag is the discriminant. An unknown
/// tag refuses the load, since v9 is not silently forward-extensible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub(crate) enum SectionDtype {
    Raw = 0,
    F32 = 1,
    F16 = 2,
    I8 = 3,
    /// Bit-packed bytes; `elem_count` counts bytes.
    BitPacked = 4,
    U32 = 5,
    U64 = 6,
    I32 = 7,
    I64 = 8,
    F64 = 9,
    U8 = 10,
}

impl SectionDtype {
    pub(crate) fn tag(self) -> u8 {
        self as u8
    }

    pub(crate) fn from_tag(tag: u8) -> Option<Self> {
        Some(match tag {
            0 => Self::Raw,
            1 => Self::F32,
            2 => Self::F16,
            3 => Self::I8,
            4 => Self::BitPacked,
            5 => Self::U32,
            6 => Self::U64,
            7 => Self::I32,
            8 => Self::I64,
            9 => Self::F64,
            10 => Self::U8,
            _ => return None,
        })
    }

    /// Bytes per element.
    pub(crate) fn width(self) -> u64 {
        match self {
            Self::Raw | Self::I8 | Self::BitPacked | Self::U8 => 1,
            Self::F16 => 2,
            Self::F32 | Self::U32 | Self::I32 => 4,
            Self::U64 | Self::I64 | Self::F64 => 8,
        }
    }
}

/// Facts about the load source that decide whether a section is mapped.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MapContext {
    pub(crate) file_backed: bool,
    pub(crate) regular_file: bool,
    /// The file is not on a network or FUSE filesystem; only `Auto` cares.
    pub(crate) local_fs: bool,
    pub(crate) windows: bool,
    pub(crate) big_endian: bool,
    pub(crate) section_len: u64,
    pub(crate) env_disabled: bool,
}

impl MapContext {
    /// The context of the running host for a section of `section_len` bytes
    /// read from `file` (`None` for a byte buffer).
    pub(crate) fn host(file: Option<&MappedFile>, section_len: u64) -> Self {
        MapContext {
            file_backed: file.is_some(),
            regular_file: file.is_some_and(MappedFile::is_regular),
            local_fs: file.is_none_or(MappedFile::is_local),
            windows: cfg!(windows),
            big_endian: cfg!(target_endian = "big"),
            section_len,
            env_disabled: mmap_disabled_by_env(),
        }
    }
}

/// Resolve a [`MapPolicy`] to "serve this section from the mapping".
///
/// `auto_min_len` is the consumer's `Auto` size floor. A byte buffer, a
/// non-regular file, a big-endian host and the environment kill switch force
/// the heap under every policy; `Auto` additionally never maps on Windows,
/// where an open mapping blocks the rename a save performs over its source,
/// nor a file on a network or FUSE filesystem, where another host's write
/// truncates pages out from under the mapping.
pub(crate) fn resolve_mapping(policy: MapPolicy, auto_min_len: u64, ctx: &MapContext) -> bool {
    if !ctx.file_backed || !ctx.regular_file || ctx.big_endian || ctx.env_disabled {
        return false;
    }
    match policy {
        MapPolicy::Memory => false,
        MapPolicy::Mapped => true,
        MapPolicy::Auto => !ctx.windows && ctx.local_fs && ctx.section_len >= auto_min_len,
    }
}

/// `false` for a file on NFS/SMB/CIFS/AFP/WebDAV/9p/FUSE. Any failure to ask,
/// and every other platform, answers `true`.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn filesystem_is_local(file: &File) -> bool {
    use std::os::fd::AsRawFd;
    // statfs(2) f_type magics of filesystems whose files another host can
    // shrink or replace under a live mapping.
    const REMOTE: [u64; 9] = [
        0x6969,     // NFS
        0x517B,     // SMB
        0xFE534D42, // SMB2
        0xFF534D42, // CIFS
        0x65735546, // FUSE
        0x01021997, // 9p
        0x5346414F, // AFS
        0x00C36400, // Ceph
        0x7461636F, // OCFS2
    ];
    // SAFETY: `statfs` is plain old data, so the all-zero value is valid.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `stat` is a valid out-pointer and the descriptor is open for the call.
    if unsafe { libc::fstatfs(file.as_raw_fd(), &mut stat) } != 0 {
        return true;
    }
    !REMOTE.contains(&((stat.f_type as u64) & 0xFFFF_FFFF))
}

#[cfg(target_os = "macos")]
fn filesystem_is_local(file: &File) -> bool {
    use std::os::fd::AsRawFd;
    // SAFETY: `statfs` is plain old data, so the all-zero value is valid.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `stat` is a valid out-pointer and the descriptor is open for the call.
    if unsafe { libc::fstatfs(file.as_raw_fd(), &mut stat) } != 0 {
        return true;
    }
    let name: Vec<u8> = stat
        .f_fstypename
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    !matches!(
        name.as_slice(),
        b"nfs" | b"smbfs" | b"afpfs" | b"webdav" | b"macfuse" | b"osxfuse" | b"fuse"
    )
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
fn filesystem_is_local(_file: &File) -> bool {
    true
}

fn mmap_disabled_by_env() -> bool {
    mmap_off(std::env::var(MMAP_ENV_VAR).ok().as_deref())
}

pub(crate) fn mmap_off(value: Option<&str>) -> bool {
    value.is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        )
    })
}

/// Whether a save to `dest` must first release this process's mappings of it.
///
/// Windows refuses to rename over a file that is mapped; Unix replaces the
/// directory entry and the old inode lives on for whoever holds the mapping.
/// Pure so the truth table runs on every OS.
pub(crate) fn must_materialize_before_save(windows: bool, dest: &Path, held: &[&Path]) -> bool {
    windows && held.contains(&dest)
}

/// A file mapped read-only for the life of its last holder.
///
/// Held through `Arc` by every section handle that points into it, so the
/// mapping outlives the graph that loaded it. On Unix a `save()` that renames
/// a new file over the path leaves this inode readable.
#[derive(Debug)]
pub(crate) struct MappedFile {
    mmap: Mmap,
    file: File,
    path: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
}

impl MappedFile {
    pub(crate) fn open(path: &Path) -> io::Result<Arc<Self>> {
        let file = File::open(path).map_err(|e| super::io_context("opening", path, e))?;
        Self::map(file, path)
    }

    /// Map an already-open `file`.
    pub(crate) fn map(file: File, path: &Path) -> io::Result<Arc<Self>> {
        let meta = file
            .metadata()
            .map_err(|e| super::io_context("reading the metadata of", path, e))?;
        let len = meta.len();
        // SAFETY: writers replace a `.kgl` atomically instead of truncating
        // it, so this inode is stable for the mapping's life. In-place
        // truncation is outside that contract; `check_intact` catches it at
        // the explicit-check points.
        let mmap = unsafe { Mmap::map(&file) }
            .map_err(|e| super::io_context(&format!("memory-mapping {len} bytes of"), path, e))?;
        Ok(Arc::new(MappedFile {
            mmap,
            file,
            path: path.to_path_buf(),
            len,
            modified: meta.modified().ok(),
        }))
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.mmap
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    fn is_regular(&self) -> bool {
        self.file.metadata().is_ok_and(|m| m.is_file())
    }

    /// Drop the resident pages inside `offset..offset + len` after a pass that
    /// touched all of them at load (checksum, UTF-8 validation). The mapping
    /// is read-only and its pages clean, so the kernel refaults them from the
    /// file on the next access and nothing is lost; only whole pages inside
    /// the range are advised, so a neighbouring section's page is never
    /// dropped. A failed call is ignored: the pages just stay resident.
    ///
    /// Linux uses `madvise(MADV_DONTNEED)`. macOS treats that call as a hint
    /// that leaves the pages in the resident set (measured: 378 MB before and
    /// after on a fully read 395 MB mapping), so it re-maps the range over
    /// itself (`MAP_FIXED`), which unmaps the resident pages from this process
    /// and leaves them in the unified buffer cache. `msync(MS_INVALIDATE)`
    /// also drops the resident set but evicts the file's cached pages, so the
    /// next open of the file reads it cold from disk (a 395 MB file booted in
    /// 2.0 s instead of 0.34 s).
    /// Windows has no equivalent and does nothing.
    pub(crate) fn release_resident(&self, offset: usize, len: usize) {
        #[cfg(unix)]
        {
            // SAFETY: `sysconf` takes no pointers.
            let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(4096) as usize;
            let base = self.mmap.as_ptr() as usize;
            let start = (base + offset).next_multiple_of(page);
            let end = (base + offset + len) / page * page;
            if end > start {
                // SAFETY: `start..end` lies inside this live read-only file
                // mapping (the section was validated against the file
                // length), so the call cannot touch another allocation, and
                // dropping clean pages of such a mapping loses no data.
                unsafe {
                    #[cfg(target_os = "macos")]
                    {
                        use std::os::fd::AsRawFd;
                        // Same file, offset and protection as the original
                        // mapping, so a failure leaves the pages resident.
                        libc::mmap(
                            start as *mut libc::c_void,
                            end - start,
                            libc::PROT_READ,
                            libc::MAP_SHARED | libc::MAP_FIXED,
                            self.file.as_raw_fd(),
                            (start - base) as libc::off_t,
                        );
                    }
                    #[cfg(not(target_os = "macos"))]
                    libc::madvise(start as *mut libc::c_void, end - start, libc::MADV_DONTNEED);
                }
            }
        }
        #[cfg(not(unix))]
        let _ = (offset, len);
    }

    /// Whether the file's filesystem is local. Unknown counts as local.
    fn is_local(&self) -> bool {
        filesystem_is_local(&self.file)
    }

    /// Fail instead of touching pages if the file is shorter than when it was
    /// mapped. A truncated mapping raises SIGBUS on access, so every explicit
    /// check point (verify, promote, save copy-out) calls this first.
    pub(crate) fn check_intact(&self) -> io::Result<()> {
        let now = self
            .file
            .metadata()
            .map_err(|e| super::io_context("re-reading the metadata of", &self.path, e))?
            .len();
        if now != self.len {
            return Err(invalid_data(format!(
                "source file was truncated or replaced: '{}' was {} bytes when it was memory-mapped and is {} now",
                self.path.display(),
                self.len,
                now
            )));
        }
        Ok(())
    }

    /// Whether the file's modification time differs from the one recorded at
    /// map time. A same-length rewrite is only visible this way.
    pub(crate) fn modified_since_open(&self) -> io::Result<bool> {
        let now = self.file.metadata()?.modified().ok();
        Ok(now != self.modified)
    }
}

/// The bytes of a container being loaded, and the mapping they live in when
/// there is one. A byte buffer, a small file read into a `Vec` and any other
/// non-mapped source carry `file: None`, which sends every mappable section
/// to the heap.
pub(crate) struct LoadSource<'a> {
    pub(crate) bytes: &'a [u8],
    pub(crate) file: Option<&'a Arc<MappedFile>>,
}

/// One row of `FileMetadata::mappable`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MapEntry {
    /// Digest key, e.g. `emb:<Type>:<prop>:data`.
    pub(crate) name: String,
    /// The compressed-section record that references this section.
    pub(crate) owner: String,
    /// Consumer-defined section class.
    pub(crate) kind: u8,
    pub(crate) dtype: u8,
    pub(crate) elem_count: u64,
    /// Relative to the data-region base.
    pub(crate) offset: u64,
    pub(crate) len: u64,
    pub(crate) flags: u32,
    pub(crate) crc32: u32,
}

/// A payload a writer registers for the data region.
#[derive(Debug, Clone)]
pub(crate) struct MappableSection<'a> {
    pub(crate) name: String,
    pub(crate) owner: String,
    pub(crate) kind: u8,
    pub(crate) dtype: SectionDtype,
    pub(crate) elem_count: u64,
    /// [`FLAG_UTF8_VALIDATED`] etc.; [`FLAG_LITTLE_ENDIAN`] is always added.
    pub(crate) flags: u32,
    pub(crate) data: Cow<'a, [u8]>,
}

/// Which container a save produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum SaveLayout {
    /// v9 when an embedding store or a property column is large enough to
    /// leave the compressed sections (the size at which a load would spill
    /// it), otherwise v8 bytes. Those buffers are written raw, so a load can
    /// map them instead of spilling a per-process copy; everything smaller
    /// stays compressed.
    #[default]
    Auto,
    /// Never emit v9: every section compressed, readable by older readers.
    Compact,
}

impl SaveLayout {
    /// Parse a user-facing layout name (`"auto"` or `"compact"`).
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "auto" => Some(SaveLayout::Auto),
            "compact" => Some(SaveLayout::Compact),
            _ => None,
        }
    }
}

/// The mappable sections a save of `graph` emits under `layout`. `Compact`
/// always yields none.
pub(crate) fn collect_mappable_sections(
    graph: &crate::graph::schema::DirGraph,
    layout: SaveLayout,
) -> io::Result<Vec<MappableSection<'_>>> {
    super::embedding_sections::collect_embedding_sections(graph, layout)
}

pub(crate) fn align_up(value: u64, align: u64) -> u64 {
    value.div_ceil(align) * align
}

/// The data-region plan for `sections`: table rows (offsets relative to the
/// region base) and the region's total length, padding included.
pub(crate) fn plan_data_region(
    sections: &[MappableSection<'_>],
) -> io::Result<(Vec<MapEntry>, u64)> {
    let mut entries: Vec<MapEntry> = Vec::with_capacity(sections.len());
    let mut cursor = 0u64;
    for section in sections {
        let len = section.data.len() as u64;
        if section.elem_count.checked_mul(section.dtype.width()) != Some(len) {
            return Err(invalid_data(format!(
                "mappable section '{}' holds {len} bytes, not {} elements of {} bytes",
                section.name,
                section.elem_count,
                section.dtype.width()
            )));
        }
        if entries.iter().any(|e| e.name == section.name) {
            return Err(invalid_data(format!(
                "mappable section '{}' is registered twice",
                section.name
            )));
        }
        let offset = align_up(cursor, SECTION_ALIGNMENT);
        cursor = offset + len;
        entries.push(MapEntry {
            name: section.name.clone(),
            owner: section.owner.clone(),
            kind: section.kind,
            dtype: section.dtype.tag(),
            elem_count: section.elem_count,
            offset,
            len,
            flags: section.flags | FLAG_LITTLE_ENDIAN,
            crc32: section_digest(&section.data),
        });
    }
    Ok((entries, cursor))
}

/// Write the data region: zero padding up to `base`, then each section at its
/// planned offset. `written` is the number of bytes already in the container.
pub(crate) fn write_data_region<W: Write>(
    writer: &mut W,
    written: u64,
    sections: &[MappableSection<'_>],
    entries: &[MapEntry],
) -> io::Result<()> {
    let mut position = written;
    let base = align_up(written, SECTION_ALIGNMENT);
    write_zeros(writer, base - position)?;
    position = base;
    for (section, entry) in sections.iter().zip(entries) {
        let target = base + entry.offset;
        write_zeros(writer, target - position)?;
        writer.write_all(&section.data)?;
        position = target + entry.len;
    }
    Ok(())
}

fn write_zeros<W: Write>(writer: &mut W, mut count: u64) -> io::Result<()> {
    const ZEROS: [u8; SECTION_ALIGNMENT as usize] = [0; SECTION_ALIGNMENT as usize];
    while count > 0 {
        let chunk = count.min(ZEROS.len() as u64) as usize;
        writer.write_all(&ZEROS[..chunk])?;
        count -= chunk as u64;
    }
    Ok(())
}

#[derive(Debug, Clone)]
enum SectionBytes {
    Mapped {
        file: Arc<MappedFile>,
        offset: usize,
        len: usize,
    },
    Heap(Arc<Vec<u8>>),
}

/// A validated section of an opened container.
///
/// A mapped handle keeps its [`MappedFile`] alive; a heap handle owns a copy.
/// Both yield identical bytes. The bytes of a mapped section start on a
/// 64-byte boundary; a heap copy has only `Vec<u8>` alignment, so a typed
/// consumer must not assume more than that from a heap handle.
#[derive(Debug, Clone)]
pub(crate) struct MappedSection {
    pub(crate) name: String,
    pub(crate) owner: String,
    pub(crate) kind: u8,
    pub(crate) dtype: SectionDtype,
    pub(crate) elem_count: u64,
    pub(crate) crc32: u32,
    bytes: SectionBytes,
}

impl MappedSection {
    pub(crate) fn is_mapped(&self) -> bool {
        matches!(self.bytes, SectionBytes::Mapped { .. })
    }

    /// Whether the mapped file's modification time differs from the one
    /// recorded at map time; `None` for a heap copy.
    pub(crate) fn modified_since_open(&self) -> Option<io::Result<bool>> {
        match &self.bytes {
            SectionBytes::Mapped { file, .. } => Some(file.modified_since_open()),
            SectionBytes::Heap(_) => None,
        }
    }

    /// The file a mapped section reads from; `None` for a heap copy.
    pub(crate) fn source_path(&self) -> Option<&Path> {
        match &self.bytes {
            SectionBytes::Mapped { file, .. } => Some(file.path()),
            SectionBytes::Heap(_) => None,
        }
    }

    /// The payload, without a truncation check. Safe to call only where the
    /// caller has already passed [`Self::checked_bytes`] or [`Self::verify`],
    /// or holds a heap handle; otherwise a file truncated behind the mapping
    /// faults on access.
    pub(crate) fn as_bytes(&self) -> &[u8] {
        match &self.bytes {
            SectionBytes::Mapped { file, offset, len } => &file.bytes()[*offset..*offset + *len],
            SectionBytes::Heap(vec) => vec,
        }
    }

    /// See [`MappedFile::release_resident`]; a heap copy has nothing to drop.
    pub(crate) fn release_resident(&self) {
        if let SectionBytes::Mapped { file, offset, len } = &self.bytes {
            file.release_resident(*offset, *len);
        }
    }

    /// The payload after confirming the backing file is still intact.
    pub(crate) fn checked_bytes(&self) -> io::Result<&[u8]> {
        if let SectionBytes::Mapped { file, .. } = &self.bytes {
            file.check_intact()?;
        }
        Ok(self.as_bytes())
    }

    /// Check the payload against the CRC32 recorded at write time.
    pub(crate) fn verify(&self) -> io::Result<()> {
        check_digest(&self.name, self.checked_bytes()?, self.crc32)
    }
}

/// Compare `bytes` with the CRC32 a writer recorded for the section `name`.
fn check_digest(name: &str, bytes: &[u8], recorded: u32) -> io::Result<()> {
    let actual = section_digest(bytes);
    if actual != recorded {
        return Err(invalid_data(format!(
            "the mappable section '{name}' of this .kgl file is corrupt — it does not match the \
             CRC32 digest recorded when the file was written (recorded {recorded:#010x}, computed \
             {actual:#010x}). Restore the file from a backup or rebuild the graph from its \
             source."
        )));
    }
    Ok(())
}

/// A mapped section shared by every clone of a column buffer, with the result
/// of its first CRC check cached so a section is verified once however many
/// snapshots hold it.
#[derive(Debug)]
pub struct SharedSection {
    section: MappedSection,
    verified: OnceLock<Result<(), String>>,
}

impl SharedSection {
    /// `None` unless `section` is served from a file mapping.
    pub(crate) fn new(section: MappedSection) -> Option<Arc<Self>> {
        section.is_mapped().then(|| {
            Arc::new(SharedSection {
                section,
                verified: OnceLock::new(),
            })
        })
    }

    pub(crate) fn name(&self) -> &str {
        &self.section.name
    }

    /// The payload, without a truncation check; see [`MappedSection::as_bytes`].
    pub(crate) fn bytes(&self) -> &[u8] {
        self.section.as_bytes()
    }

    pub(crate) fn source_path(&self) -> Option<&Path> {
        self.section.source_path()
    }

    pub(crate) fn modified_since_open(&self) -> Option<io::Result<bool>> {
        self.section.modified_since_open()
    }

    /// See [`MappedFile::release_resident`].
    pub(crate) fn release_resident(&self) {
        self.section.release_resident();
    }

    /// The CRC32 check, run once and remembered.
    pub(crate) fn verify(&self) -> io::Result<()> {
        self.verified
            .get_or_init(|| self.section.verify().map_err(|e| e.to_string()))
            .clone()
            .map_err(invalid_data)
    }
}

/// Where [`MappableTable::open_placed`] puts one section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Placement {
    /// A handle into the file mapping.
    Mapped,
    /// A handle owning a copy of the bytes.
    Heap,
    /// Neither: only the validated range is kept, for a consumer that decodes
    /// straight from the container bytes and keeps nothing of the section.
    Deferred,
}

/// A validated section the consumer decodes from the container bytes itself.
#[derive(Debug, Clone)]
pub(crate) struct DeferredSection {
    pub(crate) name: String,
    pub(crate) owner: String,
    pub(crate) kind: u8,
    pub(crate) dtype: SectionDtype,
    pub(crate) elem_count: u64,
    start: usize,
    end: usize,
}

impl DeferredSection {
    /// The payload within `container`, the bytes the table was opened over.
    pub(crate) fn bytes<'c>(&self, container: &'c [u8]) -> &'c [u8] {
        &container[self.start..self.end]
    }
}

/// The opened data region of a v9 container.
#[derive(Debug, Clone, Default)]
pub(crate) struct MappableTable {
    sections: Vec<MappedSection>,
    deferred: Vec<DeferredSection>,
    /// Section name to position in `sections` / `deferred`; a load with
    /// thousands of column sections looks each up by name.
    section_index: HashMap<String, usize>,
    deferred_index: HashMap<String, usize>,
}

impl MappableTable {
    pub(crate) fn get(&self, name: &str) -> Option<&MappedSection> {
        self.section_index.get(name).map(|&i| &self.sections[i])
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &MappedSection> {
        self.sections.iter()
    }

    pub(crate) fn get_deferred(&self, name: &str) -> Option<&DeferredSection> {
        self.deferred_index.get(name).map(|&i| &self.deferred[i])
    }

    pub(crate) fn iter_deferred(&self) -> impl Iterator<Item = &DeferredSection> {
        self.deferred.iter()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.sections.len() + self.deferred.len()
    }

    /// Verify every section's CRC32 now.
    #[cfg(test)]
    pub(crate) fn verify_all(&self) -> io::Result<()> {
        self.sections.iter().try_for_each(MappedSection::verify)
    }

    /// Check `entries` against the container without touching a section.
    ///
    /// `file_len` is the whole container's length; `base` is the absolute
    /// offset of the data region. Every check is O(table): tag and flag
    /// sanity, `len == elem_count * width`, alignment, ascending
    /// non-overlapping offsets, unique names, and that the last byte lies
    /// inside the file, so a truncated file fails here and not at first
    /// access. Returns each entry's dtype and absolute byte range.
    pub(crate) fn validate(
        entries: &[MapEntry],
        file_len: u64,
        base: u64,
    ) -> io::Result<Vec<(SectionDtype, usize, usize)>> {
        let mut ranges = Vec::with_capacity(entries.len());
        let mut previous_end = 0u64;
        for (index, entry) in entries.iter().enumerate() {
            let dtype = SectionDtype::from_tag(entry.dtype).ok_or_else(|| {
                invalid_data(format!(
                    "mappable section '{}' has unknown dtype tag {}; this build cannot read it. Please upgrade kglite.",
                    entry.name, entry.dtype
                ))
            })?;
            if entry.flags & !KNOWN_FLAGS != 0 || entry.flags & FLAG_LITTLE_ENDIAN == 0 {
                return Err(invalid_data(format!(
                    "mappable section '{}' has unsupported flags {:#x}",
                    entry.name, entry.flags
                )));
            }
            if entry.elem_count.checked_mul(dtype.width()) != Some(entry.len) {
                return Err(invalid_data(format!(
                    "mappable section '{}' declares {} bytes for {} elements of {} bytes",
                    entry.name,
                    entry.len,
                    entry.elem_count,
                    dtype.width()
                )));
            }
            let absolute = base.checked_add(entry.offset);
            if !base.is_multiple_of(SECTION_ALIGNMENT)
                || absolute.is_none_or(|a| !a.is_multiple_of(SECTION_ALIGNMENT))
            {
                return Err(invalid_data(format!(
                    "mappable section '{}' is not {SECTION_ALIGNMENT}-byte aligned",
                    entry.name
                )));
            }
            if entry.offset < previous_end {
                return Err(invalid_data(format!(
                    "mappable section '{}' overlaps or precedes the section before it",
                    entry.name
                )));
            }
            if entries[..index].iter().any(|e| e.name == entry.name) {
                return Err(invalid_data(format!(
                    "mappable section '{}' appears twice in the section table",
                    entry.name
                )));
            }
            let start = absolute.unwrap_or(u64::MAX);
            let end = start
                .checked_add(entry.len)
                .filter(|&end| end <= file_len)
                .ok_or_else(|| {
                    invalid_data(format!(
                        "file is truncated — mappable section '{}' needs {} bytes at offset {start}",
                        entry.name, entry.len
                    ))
                })?;
            previous_end = entry.offset + entry.len;
            ranges.push((dtype, start as usize, end as usize));
        }
        Ok(ranges)
    }

    /// Validate `entries` and build handles into `bytes`, the whole container.
    ///
    /// `map(entry)` decides per entry whether the handle points into `file`'s
    /// mapping (when there is one) or owns a heap copy of the bytes.
    pub(crate) fn open(
        entries: &[MapEntry],
        bytes: &[u8],
        base: u64,
        file: Option<&Arc<MappedFile>>,
        verify: Verify,
        map: impl Fn(&MapEntry) -> bool,
    ) -> io::Result<Self> {
        Self::open_placed(entries, bytes, base, file, verify, |entry| {
            if map(entry) {
                Placement::Mapped
            } else {
                Placement::Heap
            }
        })
    }

    /// [`Self::open`] with a three-way decision. A `Mapped` placement needs a
    /// `file`; without one the section is copied.
    pub(crate) fn open_placed(
        entries: &[MapEntry],
        bytes: &[u8],
        base: u64,
        file: Option<&Arc<MappedFile>>,
        verify: Verify,
        place: impl Fn(&MapEntry) -> Placement,
    ) -> io::Result<Self> {
        let ranges = Self::validate(entries, bytes.len() as u64, base)?;
        let mut sections = Vec::with_capacity(entries.len());
        let mut deferred = Vec::new();
        for (entry, (dtype, start, end)) in entries.iter().zip(ranges) {
            let placement = place(entry);
            // A copy is read here anyway and nothing re-checks it afterwards (a
            // heap handle's `verify()` is a no-op), so its CRC32 is checked in
            // the same pass; only sections that stay mapped may defer to first use.
            let copied = !matches!((placement, file), (Placement::Mapped, Some(_)));
            let checked_now =
                verify == Verify::Eager || copied || entry.flags & FLAG_UTF8_VALIDATED != 0;
            let section_bytes = match (placement, file) {
                (Placement::Deferred, _) => {
                    if checked_now {
                        check_digest(&entry.name, &bytes[start..end], entry.crc32)?;
                    }
                    deferred.push(DeferredSection {
                        name: entry.name.clone(),
                        owner: entry.owner.clone(),
                        kind: entry.kind,
                        dtype,
                        elem_count: entry.elem_count,
                        start,
                        end,
                    });
                    continue;
                }
                (Placement::Mapped, Some(file)) => SectionBytes::Mapped {
                    file: Arc::clone(file),
                    offset: start,
                    len: end - start,
                },
                _ => SectionBytes::Heap(Arc::new(bytes[start..end].to_vec())),
            };
            let section = MappedSection {
                name: entry.name.clone(),
                owner: entry.owner.clone(),
                kind: entry.kind,
                dtype,
                elem_count: entry.elem_count,
                crc32: entry.crc32,
                bytes: section_bytes,
            };
            if checked_now {
                section.verify()?;
                // A string section is validated again by its column loader,
                // which releases it after that pass; releasing here would only
                // fault it back in.
                if entry.flags & FLAG_UTF8_VALIDATED == 0 {
                    section.release_resident();
                }
            }
            sections.push(section);
        }
        let index = |names: Vec<&String>| -> HashMap<String, usize> {
            names
                .into_iter()
                .enumerate()
                .map(|(i, n)| (n.clone(), i))
                .collect()
        };
        Ok(MappableTable {
            section_index: index(sections.iter().map(|s| &s.name).collect()),
            deferred_index: index(deferred.iter().map(|s| &s.name).collect()),
            sections,
            deferred,
        })
    }
}
