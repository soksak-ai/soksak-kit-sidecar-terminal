use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use getrandom::fill as random_fill;
use sha2::{Digest, Sha256};

use crate::mirror::TerminalFrame;

const MAGIC: &[u8; 8] = b"SKTERM01";
// Payload format 2 stores the v2 frame (runs, modes, history, offset). A format-1 file fails the
// header check and surfaces as CHECKPOINT_CORRUPT; it is not migrated.
const VERSION: u8 = 2;
const NONCE_BYTES: usize = 12;
const HEADER_BYTES: usize = 8 + 1 + 8 + 8 + NONCE_BYTES;
pub const KEY_ENV: &str = "SOKSAK_TERMINAL_CHECKPOINT_KEY";

pub fn key_from_base64(encoded: &str) -> io::Result<[u8; 32]> {
    let decoded = B64
        .decode(encoded)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "checkpoint key is not base64"))?;
    decoded.try_into().map_err(|bytes: Vec<u8>| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("checkpoint key has {} bytes, expected 32", bytes.len()),
        )
    })
}

pub struct ArchivedCheckpoint {
    pub generation: u64,
    pub sequence: u64,
    pub paint: Vec<u8>,
    pub frame: TerminalFrame,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CheckpointPayload {
    paint: Vec<u8>,
    frame: TerminalFrame,
}

pub struct CheckpointStore {
    directory: PathBuf,
    lock_directory: PathBuf,
    provider: String,
    cipher: Aes256Gcm,
}

impl CheckpointStore {
    pub fn new(home: &Path, provider: &str, key: [u8; 32]) -> io::Result<Self> {
        validate_name(provider)?;
        let directory = home.join("terminal-checkpoints").join(provider);
        let lock_directory = home.join("terminal-checkpoint-locks").join(provider);
        fs::create_dir_all(&directory)?;
        fs::create_dir_all(&lock_directory)?;
        Ok(Self {
            directory,
            lock_directory,
            provider: provider.to_string(),
            cipher: Aes256Gcm::new((&key).into()),
        })
    }

    pub fn path(&self, window: &str, pane: &str) -> io::Result<PathBuf> {
        validate_coordinate(window)?;
        validate_coordinate(pane)?;
        let digest = Sha256::digest(format!("{}\0{}", window, pane).as_bytes());
        Ok(self.directory.join(format!("{digest:x}.checkpoint")))
    }

    pub fn write(
        &self,
        window: &str,
        pane: &str,
        generation: u64,
        sequence: u64,
        paint: &[u8],
        frame: &TerminalFrame,
    ) -> io::Result<()> {
        let path = self.path(window, pane)?;
        let mut nonce = [0; NONCE_BYTES];
        random_fill(&mut nonce).map_err(|error| io::Error::other(error.to_string()))?;
        let aad = self.aad(window, pane);
        let plaintext = serde_json::to_vec(&CheckpointPayload {
            paint: paint.to_vec(),
            frame: frame.clone(),
        })?;
        let ciphertext = self
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| io::Error::other("checkpoint encryption failed"))?;
        let mut bytes = Vec::with_capacity(HEADER_BYTES + ciphertext.len());
        bytes.extend_from_slice(MAGIC);
        bytes.push(VERSION);
        bytes.extend_from_slice(&generation.to_be_bytes());
        bytes.extend_from_slice(&sequence.to_be_bytes());
        bytes.extend_from_slice(&nonce);
        bytes.extend_from_slice(&ciphertext);
        self.commit(&path, generation, sequence, &bytes)
    }

    pub fn claim_generation(&self, window: &str, pane: &str, generation: u64) -> io::Result<()> {
        let path = self.path(window, pane)?;
        let mut lock = self.open_lock(&path)?;
        lock.lock()?;
        write_claimed_generation(&mut lock, Some(generation))
    }

    pub fn read(&self, window: &str, pane: &str) -> io::Result<Option<ArchivedCheckpoint>> {
        let path = self.path(window, pane)?;
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if bytes.len() < HEADER_BYTES || &bytes[..8] != MAGIC || bytes[8] != VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid checkpoint header",
            ));
        }
        let generation = u64::from_be_bytes(bytes[9..17].try_into().unwrap());
        let sequence = u64::from_be_bytes(bytes[17..25].try_into().unwrap());
        let nonce = Nonce::from_slice(&bytes[25..HEADER_BYTES]);
        let aad = self.aad(window, pane);
        let plaintext = self
            .cipher
            .decrypt(
                nonce,
                Payload {
                    msg: &bytes[HEADER_BYTES..],
                    aad: &aad,
                },
            )
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "checkpoint authentication failed",
                )
            })?;
        let payload: CheckpointPayload = serde_json::from_slice(&plaintext).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("checkpoint payload: {error}"),
            )
        })?;
        Ok(Some(ArchivedCheckpoint {
            generation,
            sequence,
            paint: payload.paint,
            frame: payload.frame,
        }))
    }

    pub fn remove(&self, window: &str, pane: &str) -> io::Result<bool> {
        let path = self.path(window, pane)?;
        let mut lock = self.open_lock(&path)?;
        lock.lock()?;
        write_claimed_generation(&mut lock, None)?;
        match fs::remove_file(path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn aad(&self, window: &str, pane: &str) -> Vec<u8> {
        format!(
            "soksak/terminal-checkpoint/v{VERSION}\0{}\0{window}\0{pane}",
            self.provider
        )
        .into_bytes()
    }

    fn open_lock(&self, path: &Path) -> io::Result<File> {
        let lock_name = path.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "checkpoint path has no file name",
            )
        })?;
        let lock_path = self.lock_directory.join(lock_name).with_extension("lock");
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(lock_path)
    }

    fn commit(&self, path: &Path, generation: u64, sequence: u64, bytes: &[u8]) -> io::Result<()> {
        let mut lock = self.open_lock(path)?;
        lock.lock()?;
        if claimed_generation(&mut lock)? != Some(generation) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "checkpoint generation does not own this pane",
            ));
        }
        if committed_position(path)?
            .is_some_and(|position| position.0 == generation && position.1 >= sequence)
        {
            return Ok(());
        }
        atomic_write(path, bytes)
    }
}

fn claimed_generation(file: &mut File) -> io::Result<Option<u64>> {
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    match bytes.len() {
        0 => Ok(None),
        8 => Ok(Some(u64::from_be_bytes(bytes.try_into().unwrap()))),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid checkpoint generation owner",
        )),
    }
}

fn write_claimed_generation(file: &mut File, generation: Option<u64>) -> io::Result<()> {
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    if let Some(generation) = generation {
        file.write_all(&generation.to_be_bytes())?;
    }
    file.sync_all()
}

fn validate_name(value: &str) -> io::Result<()> {
    let valid = !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid provider name",
        ))
    }
}

fn validate_coordinate(value: &str) -> io::Result<()> {
    if !value.is_empty() && !value.contains(['/', '\\', '\0']) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid checkpoint coordinate",
        ))
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "checkpoint path has no file name",
            )
        })?;
    let digest = Sha256::digest(bytes);
    let temporary = path.with_file_name(format!(".{file_name}.{digest:x}.tmp"));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn committed_position(path: &Path) -> io::Result<Option<(u64, u64)>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if bytes.len() < HEADER_BYTES || &bytes[..8] != MAGIC || bytes[8] != VERSION {
        return Ok(None);
    }
    let generation = u64::from_be_bytes(bytes[9..17].try_into().unwrap());
    let sequence = u64::from_be_bytes(bytes[17..25].try_into().unwrap());
    Ok(Some((generation, sequence)))
}
