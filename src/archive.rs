//! The archive container and its manifest.

use std::io::{Cursor, Read, Write};

use serde::{Deserialize, Serialize};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

use crate::classify::BackupOptions;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub const MANIFEST: &str = "manifest.json";
pub const ARCHIVE_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
pub struct LinkRecord {
    pub path: String,
    /// Tokenized, so it re-points at the target machine's home on restore.
    pub target: String,
}

#[derive(Serialize, Deserialize)]
pub struct FileRecord {
    pub path: String,
    pub sha256: String,
    pub size: usize,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub tool: String,
    pub version: u32,
    pub created_at: String,
    pub source_os: String,
    pub source_host: String,
    pub with_memory: bool,
    pub includes_credentials: bool,
    pub links: Vec<LinkRecord>,
    pub files: Vec<FileRecord>,
}

impl Manifest {
    pub fn new(created_at: String, host: String, options: &BackupOptions) -> Self {
        Self {
            tool: "claude-code-sync".into(),
            version: ARCHIVE_VERSION,
            created_at,
            source_os: std::env::consts::OS.into(),
            source_host: host,
            with_memory: options.with_memory,
            includes_credentials: options.include_credentials,
            links: Vec::new(),
            files: Vec::new(),
        }
    }
}

pub struct Entry {
    pub path: String,
    pub data: Vec<u8>,
}

pub fn write_zip(entries: &[Entry]) -> Result<Vec<u8>> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    for entry in entries {
        writer.start_file(&entry.path, options)?;
        writer.write_all(&entry.data)?;
    }

    Ok(writer.finish()?.into_inner())
}

pub fn read_zip(bytes: Vec<u8>) -> Result<Vec<Entry>> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    let mut entries = Vec::with_capacity(archive.len());

    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        if file.is_dir() {
            continue;
        }
        let path = file.name().to_string();
        // CRITICAL: the size is the archive's own claim about itself. Preallocating it verbatim
        // lets one header field ask for an arbitrary allocation, so it is capped to a hint.
        let mut data = Vec::with_capacity(file.size().min(1 << 20) as usize);
        file.read_to_end(&mut data)?;
        entries.push(Entry { path, data });
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_archive_round_trips_its_entries() {
        let entries = vec![
            Entry {
                path: "claude/CLAUDE.md".into(),
                data: b"# rules\n".to_vec(),
            },
            Entry {
                path: "agents/.skill-lock.json".into(),
                data: b"{}".to_vec(),
            },
        ];
        let bytes = write_zip(&entries).unwrap();
        let read = read_zip(bytes).unwrap();

        assert_eq!(read.len(), 2);
        assert_eq!(read[0].path, "claude/CLAUDE.md");
        assert_eq!(read[0].data, b"# rules\n");
        assert_eq!(read[1].path, "agents/.skill-lock.json");
    }

    #[test]
    fn a_binary_payload_round_trips_byte_for_byte() {
        let png = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0xff];
        let bytes = write_zip(&[Entry {
            path: "claude/x.png".into(),
            data: png.clone(),
        }])
        .unwrap();
        assert_eq!(read_zip(bytes).unwrap()[0].data, png);
    }

    #[test]
    fn reading_a_non_archive_fails_loudly() {
        assert!(read_zip(b"definitely not a zip file".to_vec()).is_err());
    }
}
