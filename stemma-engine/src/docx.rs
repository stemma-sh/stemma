use std::io::{Cursor, Read, Write};

use zip::result::ZipError;
use zip::write::FileOptions;
use zip::{ZipArchive, ZipWriter};

/// Maximum number of entries allowed in a DOCX zip archive.
/// Real DOCX files typically have 20–50 entries.
const MAX_ZIP_ENTRIES: usize = 1_000;

/// Maximum cumulative decompressed size (500 MB).
/// Prevents zip bombs from exhausting memory.
const MAX_DECOMPRESSED_BYTES: u64 = 500 * 1024 * 1024;

/// Maximum decompressed size for a single file within the archive (200 MB).
const MAX_FILE_BYTES: u64 = 200 * 1024 * 1024;

#[derive(Debug)]
pub enum DocxError {
    ZipRead(ZipError),
    ZipWrite(ZipError),
    Io(std::io::Error),
    MissingFile(String),
    ZipBomb(String),
    /// Two ZIP items carry equivalent names (OPC §7.3: ZIP item names shall be
    /// unique; equivalence is ASCII case-insensitive per OPC §6.2). Word
    /// reports such packages as corrupt; silently picking one entry would hide
    /// the corruption.
    DuplicatePartName {
        name: String,
        existing: String,
    },
    /// The ZIP writer produced a central directory that could not be safely
    /// normalized for Word compatibility. This is an engine/package invariant,
    /// never an input tolerance.
    InvalidWrittenZip(String),
}

#[derive(Clone, Debug)]
pub struct DocxFile {
    pub name: String,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct DocxArchive {
    files: Vec<DocxFile>,
}

impl DocxArchive {
    /// Build an archive directly from a list of files (useful for tests).
    pub fn from_parts(files: Vec<DocxFile>) -> Self {
        Self { files }
    }

    pub fn read(docx_bytes: &[u8]) -> Result<Self, DocxError> {
        let cursor = Cursor::new(docx_bytes);
        let mut zip = ZipArchive::new(cursor).map_err(DocxError::ZipRead)?;

        if zip.len() > MAX_ZIP_ENTRIES {
            return Err(DocxError::ZipBomb(format!(
                "archive contains {} entries (max {})",
                zip.len(),
                MAX_ZIP_ENTRIES
            )));
        }

        let mut files = Vec::with_capacity(zip.len());
        let mut total_bytes: u64 = 0;
        // OPC §7.3: ZIP item names shall be unique within the package, and
        // logical item names that differ only in ASCII case are equivalent
        // (OPC §6.2). Keyed by the lowercased name, valued by the spelling we
        // first saw, so the error can show both.
        let mut seen_names: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        for i in 0..zip.len() {
            let mut file = zip.by_index(i).map_err(DocxError::ZipRead)?;
            let name = file.name().to_string();

            if !name.ends_with('/')
                && let Some(existing) = seen_names.insert(name.to_ascii_lowercase(), name.clone())
            {
                return Err(DocxError::DuplicatePartName { name, existing });
            }

            // Reject path traversal in filenames (defense in depth).
            if name.contains("..") || name.starts_with('/') {
                return Err(DocxError::ZipBomb(format!(
                    "suspicious filename in archive: {name:?}"
                )));
            }

            // Read with per-file and cumulative size limits.
            let mut data = Vec::new();
            let mut limited = (&mut file).take(MAX_FILE_BYTES + 1);
            limited.read_to_end(&mut data).map_err(DocxError::Io)?;

            if data.len() as u64 > MAX_FILE_BYTES {
                return Err(DocxError::ZipBomb(format!(
                    "file {name:?} exceeds {} MB decompressed",
                    MAX_FILE_BYTES / (1024 * 1024)
                )));
            }

            total_bytes += data.len() as u64;
            if total_bytes > MAX_DECOMPRESSED_BYTES {
                return Err(DocxError::ZipBomb(format!(
                    "cumulative decompressed size exceeds {} MB",
                    MAX_DECOMPRESSED_BYTES / (1024 * 1024)
                )));
            }

            files.push(DocxFile { name, data });
        }
        Ok(Self { files })
    }

    pub fn write(&self) -> Result<Vec<u8>, DocxError> {
        let cursor = Cursor::new(Vec::new());
        let mut writer = ZipWriter::new(cursor);
        // Pin the per-entry modification time to a fixed epoch. With the `time`
        // feature enabled (transitively), `FileOptions::default()` stamps
        // `OffsetDateTime::now_utc()` into every local file header, so two writes
        // of the SAME package seconds apart differ in the DOS date/time fields —
        // a wall-clock leak that breaks byte-level determinism (H1). Word does
        // not read these timestamps; 1980-01-01 (the ZIP epoch,
        // `DateTime::default()`) is the reproducible-build convention.
        let options = FileOptions::default().last_modified_time(zip::DateTime::default());
        for file in &self.files {
            if file.name.ends_with('/') {
                writer
                    .add_directory(file.name.as_str(), options)
                    .map_err(DocxError::ZipWrite)?;
                continue;
            }
            writer
                .start_file(file.name.as_str(), options)
                .map_err(DocxError::ZipWrite)?;
            writer.write_all(&file.data).map_err(DocxError::Io)?;
        }
        let cursor = writer.finish().map_err(DocxError::ZipWrite)?;
        let mut bytes = cursor.into_inner();
        normalize_word_compatible_creator_version(&mut bytes)?;
        Ok(bytes)
    }

    /// Look up a part by name. Part-name equivalence is ASCII case-insensitive
    /// (OPC §6.2), so a lookup for `word/document.xml` resolves a part stored
    /// as `word/Document.xml`. An exact match wins; `read` guarantees at most
    /// one case-equivalent entry exists.
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.find(name).map(|file| file.data.as_slice())
    }

    fn find(&self, name: &str) -> Option<&DocxFile> {
        self.files
            .iter()
            .find(|file| file.name == name)
            .or_else(|| {
                self.files
                    .iter()
                    .find(|file| file.name.eq_ignore_ascii_case(name))
            })
    }

    fn find_mut(&mut self, name: &str) -> Option<&mut DocxFile> {
        let idx = self
            .files
            .iter()
            .position(|file| file.name == name)
            .or_else(|| {
                self.files
                    .iter()
                    .position(|file| file.name.eq_ignore_ascii_case(name))
            })?;
        Some(&mut self.files[idx])
    }

    /// Replace an existing part's bytes. Resolves the name case-insensitively
    /// (OPC §6.2) and keeps the stored spelling, so relationship targets that
    /// point at the original spelling stay valid.
    pub fn set(&mut self, name: &str, data: Vec<u8>) -> Result<(), DocxError> {
        if let Some(file) = self.find_mut(name) {
            file.data = data;
            return Ok(());
        }
        Err(DocxError::MissingFile(name.to_string()))
    }

    /// Insert or replace a part. Resolves case-insensitively like [`Self::set`]
    /// (keeping the stored spelling on replace) so an upsert can never create a
    /// case-equivalent duplicate of an existing part (OPC §7.3).
    pub fn upsert(&mut self, name: &str, data: Vec<u8>) {
        if let Some(file) = self.find_mut(name) {
            file.data = data;
            return;
        }
        self.files.push(DocxFile {
            name: name.to_string(),
            data,
        });
    }

    pub fn list(&self) -> impl Iterator<Item = &str> {
        self.files.iter().map(|file| file.name.as_str())
    }
}

/// Normalize the central-directory "version made by" value emitted by zip
/// 0.6.6 from 4.6 to 2.0 while preserving its creator-OS byte.
///
/// The crate hardcodes 4.6 for every entry even when the archive uses only
/// classic ZIP 2.0 features. Real Word can then open the package without a
/// repair prompt yet silently ignore a part (an oracle witness ignored
/// `word/styles.xml`, shifting every framed running head) until Word resaved
/// the archive with creator version 2.0. The field describes the writer, not
/// the compression feature requirement; each local/central "version needed"
/// remains untouched.
///
/// This parser is deliberately bounded to the classic ZIP shape this writer
/// emits. DOCX read limits keep entries and sizes below ZIP64 thresholds. Any
/// mismatch is our own broken-output invariant and fails the write loudly.
fn normalize_word_compatible_creator_version(bytes: &mut [u8]) -> Result<(), DocxError> {
    const EOCD_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
    const CENTRAL_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];
    const EOCD_MIN_SIZE: usize = 22;
    const MAX_ZIP_COMMENT: usize = u16::MAX as usize;

    if bytes.len() < EOCD_MIN_SIZE {
        return Err(DocxError::InvalidWrittenZip(
            "ZIP is shorter than the end-of-central-directory record".to_string(),
        ));
    }
    let search_start = bytes.len().saturating_sub(EOCD_MIN_SIZE + MAX_ZIP_COMMENT);
    let eocd = (search_start..=bytes.len() - EOCD_MIN_SIZE)
        .rev()
        .find(|offset| bytes[*offset..].starts_with(&EOCD_SIGNATURE))
        .ok_or_else(|| {
            DocxError::InvalidWrittenZip(
                "ZIP end-of-central-directory signature is missing".to_string(),
            )
        })?;
    let disk = read_zip_u16(bytes, eocd + 4);
    let central_disk = read_zip_u16(bytes, eocd + 6);
    let disk_entries = read_zip_u16(bytes, eocd + 8);
    let total_entries = read_zip_u16(bytes, eocd + 10);
    let central_size = read_zip_u32(bytes, eocd + 12) as usize;
    let central_offset = read_zip_u32(bytes, eocd + 16) as usize;
    let comment_len = read_zip_u16(bytes, eocd + 20) as usize;
    if disk != 0 || central_disk != 0 || disk_entries != total_entries {
        return Err(DocxError::InvalidWrittenZip(format!(
            "expected a single-disk ZIP, got disk={disk} central_disk={central_disk} entries={disk_entries}/{total_entries}"
        )));
    }
    if eocd + EOCD_MIN_SIZE + comment_len != bytes.len() {
        return Err(DocxError::InvalidWrittenZip(format!(
            "ZIP comment length does not reach end of archive: eocd={eocd} comment_len={comment_len} bytes={}",
            bytes.len()
        )));
    }
    let central_end = central_offset.checked_add(central_size).ok_or_else(|| {
        DocxError::InvalidWrittenZip("central-directory size overflowed usize".to_string())
    })?;
    if central_end != eocd {
        return Err(DocxError::InvalidWrittenZip(format!(
            "central directory does not end at EOCD: offset={central_offset} size={central_size} eocd={eocd}"
        )));
    }

    let mut cursor = central_offset;
    for index in 0..usize::from(total_entries) {
        if cursor + 46 > central_end || !bytes[cursor..].starts_with(&CENTRAL_SIGNATURE) {
            return Err(DocxError::InvalidWrittenZip(format!(
                "central-directory entry {index} is truncated or has the wrong signature at offset {cursor}"
            )));
        }
        // Low byte = creator version, high byte = creator operating system.
        let file_name_len = read_zip_u16(bytes, cursor + 28) as usize;
        let extra_len = read_zip_u16(bytes, cursor + 30) as usize;
        let comment_len = read_zip_u16(bytes, cursor + 32) as usize;
        bytes[cursor + 4] = 20;
        cursor = cursor
            .checked_add(46 + file_name_len + extra_len + comment_len)
            .ok_or_else(|| {
                DocxError::InvalidWrittenZip(format!(
                    "central-directory entry {index} length overflowed usize"
                ))
            })?;
    }
    if cursor != central_end {
        return Err(DocxError::InvalidWrittenZip(format!(
            "central-directory entry count did not consume its declared size: cursor={cursor} end={central_end} entries={total_entries}"
        )));
    }
    Ok(())
}

fn read_zip_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_zip_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zip_with_entries(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zip = ZipWriter::new(Cursor::new(&mut buf));
            let opts: FileOptions = FileOptions::default();
            for (name, data) in entries {
                zip.start_file(*name, opts).unwrap();
                zip.write_all(data).unwrap();
            }
            zip.finish().unwrap();
        }
        buf
    }

    #[test]
    fn read_rejects_exact_duplicate_part_name() {
        let bytes = zip_with_entries(&[
            ("word/document.xml", b"<a/>"),
            ("word/document.xml", b"<b/>"),
        ]);
        let err = DocxArchive::read(&bytes).expect_err("duplicate part names must be rejected");
        assert!(
            matches!(err, DocxError::DuplicatePartName { ref name, ref existing }
                if name == "word/document.xml" && existing == "word/document.xml"),
            "expected DuplicatePartName, got {err:?}"
        );
    }

    #[test]
    fn read_rejects_case_equivalent_duplicate_part_name() {
        // OPC §6.2: names differing only in ASCII case are equivalent.
        let bytes = zip_with_entries(&[
            ("word/document.xml", b"<a/>"),
            ("word/Document.xml", b"<b/>"),
        ]);
        let err = DocxArchive::read(&bytes)
            .expect_err("case-equivalent duplicate part names must be rejected");
        assert!(
            matches!(err, DocxError::DuplicatePartName { ref name, ref existing }
                if name == "word/Document.xml" && existing == "word/document.xml"),
            "expected DuplicatePartName, got {err:?}"
        );
    }

    #[test]
    fn get_resolves_part_name_case_insensitively() {
        let bytes = zip_with_entries(&[("word/Document.xml", b"<doc/>")]);
        let archive = DocxArchive::read(&bytes).unwrap();
        assert_eq!(archive.get("word/document.xml"), Some(b"<doc/>".as_slice()));
        assert_eq!(archive.get("word/Document.xml"), Some(b"<doc/>".as_slice()));
        assert_eq!(archive.get("word/styles.xml"), None);
    }

    #[test]
    fn set_and_upsert_keep_the_stored_spelling() {
        let bytes = zip_with_entries(&[("word/Document.xml", b"<doc/>")]);
        let mut archive = DocxArchive::read(&bytes).unwrap();

        archive
            .set("word/document.xml", b"<set/>".to_vec())
            .expect("set must resolve the case-equivalent stored part");
        archive.upsert("word/document.xml", b"<upserted/>".to_vec());

        // One part, stored spelling preserved (rels Targets point at it).
        let names: Vec<&str> = archive.list().collect();
        assert_eq!(names, vec!["word/Document.xml"]);
        assert_eq!(
            archive.get("word/document.xml"),
            Some(b"<upserted/>".as_slice())
        );
    }

    #[test]
    fn write_uses_word_compatible_creator_version_without_disabling_compression() {
        let archive = DocxArchive::from_parts(vec![
            DocxFile {
                name: "[Content_Types].xml".to_string(),
                data: b"<Types/>".to_vec(),
            },
            DocxFile {
                name: "word/document.xml".to_string(),
                data: b"<w:document/>".to_vec(),
            },
            DocxFile {
                name: "word/_rels/document.xml.rels".to_string(),
                data: b"<Relationships/>".to_vec(),
            },
            DocxFile {
                name: "word/media/image1.bin".to_string(),
                data: vec![0; 1024],
            },
        ]);

        let bytes = archive.write().expect("write package");
        let mut zip = ZipArchive::new(Cursor::new(bytes)).expect("open written package");
        for name in [
            "[Content_Types].xml",
            "word/document.xml",
            "word/_rels/document.xml.rels",
            "word/media/image1.bin",
        ] {
            assert_eq!(
                zip.by_name(name).expect("part").version_made_by(),
                (2, 0),
                "every central-directory creator version must be Word-compatible: {name}"
            );
        }
        assert_eq!(
            zip.by_name("word/media/image1.bin")
                .expect("binary part")
                .compression(),
            zip::CompressionMethod::Deflated,
            "normal package compression remains enabled"
        );
    }
}
