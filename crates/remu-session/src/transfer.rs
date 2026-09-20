//! Streaming file transfer over the bulk data channel.
//!
//! Both halves stream. The predecessor did neither: it base64-encoded every
//! chunk into JSON on the way out (a 33% bandwidth tax and a parse per chunk),
//! and on the way in it accumulated every chunk in an array and only built the
//! file once the last one arrived — so receiving a file larger than the
//! available memory killed the app. Here the sender reads the file a chunk at
//! a time and the receiver writes each chunk straight to a temporary file, so
//! peak memory is one chunk in each direction regardless of file size.
//!
//! # Protocol
//!
//! The control channel carries the envelope —
//! [`ControlMessage::FileMeta`], `FileAccept`/`FileRefuse`, `FileEnd`,
//! `FileCancel` — and the bulk channel carries the bytes, framed by
//! [`remu_proto::bulk`]. This module owns the bytes and the disk; wiring the
//! control messages is the application's job, because only it knows whether
//! the user accepted the file.
//!
//! # Trust
//!
//! Everything about an incoming file is peer-supplied: its name, its size and
//! its digest. The name is sanitized, the sequence numbers are checked, the
//! declared size is enforced and the digest is verified before the file is
//! given its real name.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufWriter};
use uuid::Uuid;

use remu_proto::{sanitize_filename, ControlMessage, TransferError, BULK_CHUNK_BYTES};

use crate::error::SessionError;

/// Bytes allowed to be outstanding on the bulk channel before the sender
/// pauses.
///
/// Sixty-four chunks. Big enough to keep the link saturated across one
/// round trip, small enough that cancelling a transfer stops the bytes almost
/// immediately instead of after megabytes of already-queued data.
pub const BULK_HIGH_WATER_BYTES: usize = 64 * BULK_CHUNK_BYTES;

/// How long the sender waits before re-checking the send buffer.
const DRAIN_POLL: Duration = Duration::from_millis(10);

/// Suffix attempts before giving up on finding an unused filename.
const MAX_NAME_ATTEMPTS: u32 = 1_000;

/// The MIME type announced for outgoing files.
///
/// Remu does not sniff content and does not map extensions: the receiver picks
/// a handler from the filename like every other download, so announcing a
/// guessed type would be a claim nothing here can back up.
const DEFAULT_MIME: &str = "application/octet-stream";

/// Where the bytes of a transfer go.
///
/// A trait rather than a direct [`crate::PeerSession`] dependency so the
/// transfer logic — which is where the interesting failure modes live — can be
/// driven from a test without standing up a peer connection.
#[async_trait::async_trait]
pub trait BulkSink: Send + Sync {
    /// Sends one framed chunk.
    async fn send_bulk(
        &self,
        transfer_id: Uuid,
        seq: u64,
        payload: &[u8],
    ) -> Result<(), SessionError>;

    /// Bytes queued but not yet released by the transport.
    fn buffered_bulk_bytes(&self) -> usize;
}

#[async_trait::async_trait]
impl BulkSink for crate::PeerSession {
    async fn send_bulk(
        &self,
        transfer_id: Uuid,
        seq: u64,
        payload: &[u8],
    ) -> Result<(), SessionError> {
        crate::PeerSession::send_bulk(self, transfer_id, seq, payload).await
    }

    fn buffered_bulk_bytes(&self) -> usize {
        crate::PeerSession::buffered_bulk_bytes(self)
    }
}

/// A shared "stop" flag, handed to the UI so a user can abandon a transfer.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// How far along a transfer is, in the form a progress bar wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferProgress {
    pub transfer_id: Uuid,
    pub name: String,
    pub size: u64,
    pub transferred: u64,
    /// True for a file leaving this machine.
    pub outgoing: bool,
    pub state: TransferState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferState {
    /// Announced, waiting for the peer to accept.
    Pending,
    /// Bytes are moving.
    Active,
    /// All bytes moved and, on the receiving side, the digest verified.
    Complete,
    Failed(TransferError),
    Cancelled,
}

/// Streams one file out, a chunk at a time.
#[derive(Debug)]
pub struct FileSender {
    transfer_id: Uuid,
    path: PathBuf,
    name: String,
    size: u64,
    cancel: CancelToken,
}

impl FileSender {
    /// Opens `path` to read its size and name. The contents are not read until
    /// [`stream`](Self::stream).
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, SessionError> {
        let path = path.as_ref().to_path_buf();
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|source| io_error(&path, source))?;
        if metadata.is_dir() {
            return Err(io_error(
                &path,
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "directories cannot be sent",
                ),
            ));
        }
        // The local filename is sanitized too: it is about to become a
        // peer-supplied name at the far end, and sending something the receiver
        // would only have to strip is pointless.
        let name = sanitize_filename(&path.file_name().unwrap_or_default().to_string_lossy());

        Ok(Self {
            transfer_id: Uuid::new_v4(),
            path,
            name,
            size: metadata.len(),
            cancel: CancelToken::new(),
        })
    }

    pub fn transfer_id(&self) -> Uuid {
        self.transfer_id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// A handle that stops [`stream`](Self::stream) from anywhere.
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// The announcement to put on the control channel before streaming.
    pub fn file_meta(&self) -> ControlMessage {
        ControlMessage::FileMeta {
            transfer_id: self.transfer_id,
            name: self.name.clone(),
            size: self.size,
            mime: DEFAULT_MIME.to_string(),
        }
    }

    /// The completion message, built from the digest [`stream`](Self::stream)
    /// returned.
    pub fn file_end(&self, sha256: String) -> ControlMessage {
        ControlMessage::FileEnd {
            transfer_id: self.transfer_id,
            sha256,
        }
    }

    pub fn progress(&self, transferred: u64, state: TransferState) -> TransferProgress {
        TransferProgress {
            transfer_id: self.transfer_id,
            name: self.name.clone(),
            size: self.size,
            transferred,
            outgoing: true,
            state,
        }
    }

    /// Reads the file and pushes it through `sink`, returning the hex SHA-256
    /// of everything sent.
    ///
    /// The digest is computed from the bytes as they stream, so it describes
    /// what was actually sent rather than a second read of a file that may have
    /// changed underneath us.
    pub async fn stream<S: BulkSink + ?Sized>(
        &self,
        sink: &S,
        progress: &mut dyn FnMut(TransferProgress),
    ) -> Result<String, SessionError> {
        let mut file = File::open(&self.path)
            .await
            .map_err(|source| io_error(&self.path, source))?;

        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; BULK_CHUNK_BYTES];
        let mut seq = 0u64;
        let mut transferred = 0u64;

        progress(self.progress(0, TransferState::Active));

        loop {
            self.check_cancelled(transferred, progress)?;
            while sink.buffered_bulk_bytes() > BULK_HIGH_WATER_BYTES {
                self.check_cancelled(transferred, progress)?;
                tokio::time::sleep(DRAIN_POLL).await;
            }

            let read = read_chunk(&mut file, &mut buffer)
                .await
                .map_err(|source| io_error(&self.path, source))?;
            if read == 0 {
                break;
            }

            hasher.update(&buffer[..read]);
            if let Err(err) = sink.send_bulk(self.transfer_id, seq, &buffer[..read]).await {
                // `Cancelled` is the closest thing the protocol has to "the
                // sender gave up": from the far side a transport failure here
                // is indistinguishable from the user pressing stop.
                progress(
                    self.progress(transferred, TransferState::Failed(TransferError::Cancelled)),
                );
                return Err(err);
            }

            seq += 1;
            transferred += read as u64;
            progress(self.progress(transferred, TransferState::Active));
        }

        progress(self.progress(transferred, TransferState::Complete));
        Ok(hex::encode(hasher.finalize()))
    }

    fn check_cancelled(
        &self,
        transferred: u64,
        progress: &mut dyn FnMut(TransferProgress),
    ) -> Result<(), SessionError> {
        if !self.cancel.is_cancelled() {
            return Ok(());
        }
        progress(self.progress(transferred, TransferState::Cancelled));
        Err(SessionError::TransferCancelled {
            transfer_id: self.transfer_id,
        })
    }
}

/// Receives one file, writing straight to disk.
///
/// The bytes land in a hidden `.part` file in the destination directory and are
/// only renamed to the real name once the digest verifies, so an interrupted or
/// corrupt transfer never leaves a plausible-looking file behind.
#[derive(Debug)]
pub struct FileReceiver {
    /// Set once the bytes have been renamed into place, so `Drop` knows the
    /// temporary is no longer ours to remove.
    published: bool,
    transfer_id: Uuid,
    /// Sanitized at construction; the peer's original string is never used to
    /// build a path.
    name: String,
    size: u64,
    dir: PathBuf,
    temp: PathBuf,
    /// `None` once the handle has been closed. Taking it rather than holding
    /// it to the end lets both `discard` and `Drop` close the file *before*
    /// unlinking, which Windows requires and Unix does not mind.
    file: Option<BufWriter<File>>,
    hasher: Sha256,
    received: u64,
    next_seq: u64,
}

impl FileReceiver {
    /// Creates the temporary file for an announced transfer.
    ///
    /// `name` is the peer's — it is sanitized here and never used raw.
    pub async fn create(
        dir: impl AsRef<Path>,
        transfer_id: Uuid,
        name: &str,
        size: u64,
    ) -> Result<Self, SessionError> {
        let dir = dir.as_ref().to_path_buf();
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|source| io_error(&dir, source))?;

        let temp = dir.join(format!(".remu-{transfer_id}.part"));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .await
            .map_err(|source| io_error(&temp, source))?;

        Ok(Self {
            transfer_id,
            name: sanitize_filename(name),
            size,
            dir,
            temp,
            file: Some(BufWriter::new(file)),
            hasher: Sha256::new(),
            received: 0,
            next_seq: 0,
            published: false,
        })
    }

    pub fn transfer_id(&self) -> Uuid {
        self.transfer_id
    }

    /// The sanitized name the file will be given.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn received(&self) -> u64 {
        self.received
    }

    /// The `.part` file currently being written.
    pub fn temp_path(&self) -> &Path {
        &self.temp
    }

    pub fn progress(&self, state: TransferState) -> TransferProgress {
        TransferProgress {
            transfer_id: self.transfer_id,
            name: self.name.clone(),
            size: self.size,
            transferred: self.received,
            outgoing: false,
            state,
        }
    }

    /// Appends one chunk.
    ///
    /// The bulk channel is ordered and reliable, so a sequence number that is
    /// not the next one means a bug or a hostile peer; the chunk is refused
    /// rather than written at the wrong offset. Writing more than the announced
    /// size is refused for the same reason — an unbounded "file" would fill the
    /// disk.
    pub async fn write_chunk(&mut self, seq: u64, payload: &[u8]) -> Result<(), SessionError> {
        if seq != self.next_seq {
            return Err(SessionError::ChunkOutOfOrder {
                transfer_id: self.transfer_id,
                expected: self.next_seq,
                got: seq,
            });
        }
        let after = self.received.saturating_add(payload.len() as u64);
        if after > self.size {
            return Err(SessionError::TransferOversized {
                transfer_id: self.transfer_id,
                received: after,
                declared: self.size,
            });
        }

        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io_error(&self.temp, std::io::ErrorKind::BrokenPipe.into()))?;
        file.write_all(payload)
            .await
            .map_err(|source| io_error(&self.temp, source))?;
        self.hasher.update(payload);
        self.received = after;
        self.next_seq += 1;
        Ok(())
    }

    /// Verifies the sender's digest and moves the file into place.
    ///
    /// Returns the path the file was given, which is not necessarily
    /// [`name`](Self::name): an existing file is never overwritten, so a
    /// collision becomes `report (2).pdf`.
    pub async fn finish(mut self, sha256: &str) -> Result<PathBuf, SessionError> {
        // Every failure below is funnelled through one discard. Returning an
        // error straight out of `finish` used to leak the `.part` file: the
        // method consumes `self`, so the caller had no handle left to clean up
        // with, and a peer that could make the publish step fail — an
        // over-long name was enough — could park a full-size temporary in the
        // download directory per transfer until the disk was full.
        match self.publish(sha256).await {
            Ok(destination) => Ok(destination),
            Err(err) => {
                self.discard().await;
                Err(err)
            }
        }
    }

    /// The body of [`finish`](Self::finish), by reference so the caller can
    /// always clean up after it.
    async fn publish(&mut self, sha256: &str) -> Result<PathBuf, SessionError> {
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io_error(&self.temp, std::io::ErrorKind::BrokenPipe.into()))?;
        file.flush()
            .await
            .map_err(|source| io_error(&self.temp, source))?;

        // Both halves of the announcement are checked, not just the digest: a
        // sender can declare 10 MB, send 1 MB and hand over an honest digest of
        // that 1 MB, and the file would land looking complete while the UI had
        // been counting up to 10 MB.
        if self.received != self.size {
            return Err(SessionError::TransferTruncated {
                transfer_id: self.transfer_id,
                received: self.received,
                declared: self.size,
            });
        }

        let computed = hex::encode(self.hasher.finalize_reset());
        if !computed.eq_ignore_ascii_case(sha256.trim()) {
            // The bytes are wrong, so nothing of this transfer may survive:
            // a half-right file with a plausible name is worse than none.
            return Err(SessionError::ChecksumMismatch {
                transfer_id: self.transfer_id,
                expected: sha256.to_string(),
                computed,
            });
        }

        let destination = reserve_name(&self.dir, &self.name).await?;
        // Renames over the reservation made above, which is what makes
        // "pick a free name" and "take that name" one step from any other
        // process's point of view.
        if let Err(source) = tokio::fs::rename(&self.temp, &destination).await {
            // The reservation is an empty file at the real name. Left behind
            // after a failed rename it is worse than nothing: the user sees a
            // zero-byte `report.pdf`, and the retry is disambiguated to
            // `report (2).pdf` because the bad name is now taken.
            if let Err(err) = tokio::fs::remove_file(&destination).await {
                tracing::warn!(
                    %err,
                    path = %destination.display(),
                    "could not remove the reservation for a download that failed to land"
                );
            }
            return Err(io_error(&destination, source));
        }
        self.published = true;
        Ok(destination)
    }

    /// Marks the temporary as published so `Drop` leaves it alone.
    fn disarm(&mut self) {
        self.published = true;
    }

    /// Abandons the transfer and removes the partial file.
    pub async fn discard(mut self) {
        let temp = self.temp.clone();
        // Close before unlinking, then disarm so `Drop` does not try again.
        self.file.take();
        self.disarm();
        if let Err(err) = tokio::fs::remove_file(&temp).await {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::debug!(%err, path = %temp.display(), "could not remove a partial download");
            }
        }
    }
}

impl Drop for FileReceiver {
    /// Removes the `.part` file if the receiver is dropped without finishing.
    ///
    /// `finish` and `discard` clean up their own paths, but a receiver dropped
    /// anywhere else — a `write_chunk` that returned `ChunkOutOfOrder`, a
    /// session that ended mid-transfer, a `?` in a caller — would otherwise
    /// leave a partial download sitting in the user's directory. A peer that
    /// can start transfers and abandon them could fill a disk that way.
    ///
    /// Synchronous `std::fs` on purpose: `Drop` cannot await, and spawning a
    /// task would race the temp file against process exit.
    fn drop(&mut self) {
        if self.published {
            return;
        }
        // Close the handle first; Windows refuses to unlink an open file.
        self.file.take();
        if let Err(err) = std::fs::remove_file(&self.temp) {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::debug!(
                    %err,
                    path = %self.temp.display(),
                    "could not remove an abandoned partial download"
                );
            }
        }
    }
}

/// Reads up to `buffer.len()` bytes, short only at end of file.
///
/// `AsyncRead::read` may legally return fewer bytes than asked for at any
/// point; taking that as end-of-file would cut the file short, and taking it as
/// a chunk boundary would send far more, far smaller chunks than the framing is
/// sized for.
async fn read_chunk(file: &mut File, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        let read = file.read(&mut buffer[filled..]).await?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    Ok(filled)
}

/// Claims an unused path in `dir` by creating the file, so that the later
/// rename cannot clobber something that appeared in between.
async fn reserve_name(dir: &Path, name: &str) -> Result<PathBuf, SessionError> {
    for attempt in 1..=MAX_NAME_ATTEMPTS {
        let candidate = dir.join(if attempt == 1 {
            name.to_string()
        } else {
            disambiguate(name, attempt)
        });
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
            .await
        {
            // Closed straight away: the reservation is the directory entry,
            // and the rename in `finish` replaces it.
            Ok(file) => {
                drop(file);
                return Ok(candidate);
            }
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(io_error(&candidate, source)),
        }
    }
    Err(SessionError::NoFreeFilename {
        name: name.to_string(),
        dir: dir.display().to_string(),
    })
}

/// `report.pdf` + 2 -> `report (2).pdf`; a name with no extension just gets the
/// suffix. The split is on the last dot, matching what a file manager shows as
/// the extension.
fn disambiguate(name: &str, attempt: u32) -> String {
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => format!("{stem} ({attempt}).{extension}"),
        _ => format!("{name} ({attempt})"),
    }
}

fn io_error(path: &Path, source: std::io::Error) -> SessionError {
    SessionError::Io {
        path: path.display().to_string(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use sha2::Digest;
    use std::sync::atomic::AtomicUsize;

    /// Collects what the sender produced, and can pretend the transport is
    /// congested so the back-pressure path is exercised.
    #[derive(Default)]
    struct RecordingSink {
        chunks: Mutex<Vec<(u64, Vec<u8>)>>,
        buffered: AtomicUsize,
        /// Each call to `buffered_bulk_bytes` decrements this; while it is
        /// positive the sink claims to be over the high-water mark.
        congested_polls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl BulkSink for RecordingSink {
        async fn send_bulk(
            &self,
            _transfer_id: Uuid,
            seq: u64,
            payload: &[u8],
        ) -> Result<(), SessionError> {
            self.chunks.lock().push((seq, payload.to_vec()));
            Ok(())
        }

        fn buffered_bulk_bytes(&self) -> usize {
            if self
                .congested_polls
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |left| {
                    left.checked_sub(1)
                })
                .is_ok()
            {
                return BULK_HIGH_WATER_BYTES + 1;
            }
            self.buffered.load(Ordering::Acquire)
        }
    }

    fn digest(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    fn body(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    async fn write_temp_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        tokio::fs::write(&path, bytes).await.expect("write fixture");
        path
    }

    #[tokio::test]
    async fn the_receiver_lands_the_exact_bytes_on_disk_and_names_the_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let bytes = body(3 * BULK_CHUNK_BYTES + 17);
        let id = Uuid::new_v4();

        let mut receiver = FileReceiver::create(dir.path(), id, "report.pdf", bytes.len() as u64)
            .await
            .expect("receiver");
        for (seq, chunk) in bytes.chunks(BULK_CHUNK_BYTES).enumerate() {
            receiver
                .write_chunk(seq as u64, chunk)
                .await
                .expect("chunk accepted");
        }
        let path = receiver.finish(&digest(&bytes)).await.expect("verified");

        assert_eq!(path, dir.path().join("report.pdf"));
        assert_eq!(tokio::fs::read(&path).await.expect("read back"), bytes);
        // Nothing but the finished file: the .part must be gone.
        let mut entries = tokio::fs::read_dir(dir.path()).await.expect("list");
        let mut names = vec![];
        while let Some(entry) = entries.next_entry().await.expect("entry") {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        assert_eq!(names, vec!["report.pdf".to_string()]);
    }

    #[tokio::test]
    async fn a_wrong_digest_is_rejected_and_the_partial_file_is_removed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let bytes = body(4_000);
        let mut receiver =
            FileReceiver::create(dir.path(), Uuid::new_v4(), "x.bin", bytes.len() as u64)
                .await
                .expect("receiver");
        receiver.write_chunk(0, &bytes).await.expect("chunk");
        let temp = receiver.temp_path().to_path_buf();

        let err = receiver
            .finish(&digest(b"some other content"))
            .await
            .expect_err("the digest does not match");
        assert!(
            matches!(err, SessionError::ChecksumMismatch { .. }),
            "unexpected: {err}"
        );
        assert!(!temp.exists(), "the partial file was left behind");
        assert!(
            !dir.path().join("x.bin").exists(),
            "a bad file was published"
        );
    }

    /// A sender may declare 10 MB, send 1 MB and then hand over an honest
    /// digest of the megabyte it sent. Only the digest used to be checked, so
    /// the short file landed looking complete.
    #[tokio::test]
    async fn a_transfer_that_ends_short_of_its_declared_size_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let bytes = body(1_000);
        let mut receiver =
            FileReceiver::create(dir.path(), Uuid::new_v4(), "short.bin", 10_000_000)
                .await
                .expect("receiver");
        receiver.write_chunk(0, &bytes).await.expect("chunk");
        let temp = receiver.temp_path().to_path_buf();

        let err = receiver
            .finish(&digest(&bytes))
            .await
            .expect_err("a megabyte short of the announcement");
        assert!(
            matches!(
                err,
                SessionError::TransferTruncated {
                    received: 1_000,
                    declared: 10_000_000,
                    ..
                }
            ),
            "unexpected: {err}"
        );
        assert!(!temp.exists(), "the partial file was left behind");
        assert!(
            !dir.path().join("short.bin").exists(),
            "a short file was published"
        );
    }

    /// `finish` consumes the receiver, so a failure after the temporary is
    /// full has to clean up on its own — there is no handle left to discard
    /// with. A peer able to force this repeatedly could otherwise fill the
    /// disk with orphaned `.part` files.
    #[tokio::test]
    async fn a_failure_after_the_digest_verifies_still_removes_the_partial_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let bytes = body(4_000);
        let mut receiver =
            FileReceiver::create(dir.path(), Uuid::new_v4(), "x.bin", bytes.len() as u64)
                .await
                .expect("receiver");
        receiver.write_chunk(0, &bytes).await.expect("chunk");
        let temp = receiver.temp_path().to_path_buf();

        // Every name the receiver is willing to pick is already taken, so
        // reservation fails after the digest has verified.
        for attempt in 1..=MAX_NAME_ATTEMPTS {
            let name = if attempt == 1 {
                "x.bin".to_string()
            } else {
                disambiguate("x.bin", attempt)
            };
            std::fs::write(dir.path().join(name), b"taken").expect("occupy the name");
        }

        let err = receiver
            .finish(&digest(&bytes))
            .await
            .expect_err("no name is free");
        assert!(
            matches!(err, SessionError::NoFreeFilename { .. }),
            "unexpected: {err}"
        );
        assert!(
            !temp.exists(),
            "the fully received .part file was orphaned in the download directory"
        );
    }

    /// A receiver dropped without finishing must not leave its partial file.
    ///
    /// `finish` and `discard` clean up their own paths, but `write_chunk`
    /// errors return without consuming the receiver, and a session can end
    /// mid-transfer. Without the `Drop` net a peer could start transfers,
    /// abandon them, and fill the user's disk with `.part` files.
    #[tokio::test]
    async fn a_receiver_dropped_mid_transfer_leaves_nothing_behind() {
        let dir = tempfile::tempdir().expect("temp dir");
        let temp = {
            let mut receiver =
                FileReceiver::create(dir.path(), Uuid::new_v4(), "report.pdf", 4_000)
                    .await
                    .expect("receiver");
            receiver.write_chunk(0, &body(2_000)).await.expect("chunk");
            let temp = receiver.temp_path().to_path_buf();
            assert!(
                temp.exists(),
                "the partial file should exist while receiving"
            );
            temp
            // `receiver` is dropped here, mid-transfer.
        };
        assert!(!temp.exists(), "a partial download was left behind on drop");

        let mut entries = tokio::fs::read_dir(dir.path()).await.expect("list");
        assert!(
            entries.next_entry().await.expect("entry").is_none(),
            "the download directory should be empty"
        );
    }

    /// The reservation is an empty file at the real name. If the rename then
    /// fails it must go: a zero-byte `report.pdf` is a worse outcome than an
    /// error, and it also forces the retry to land as `report (2).pdf`.
    #[tokio::test]
    async fn a_rename_that_fails_publishes_nothing_under_the_real_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        let bytes = body(2_000);
        let mut receiver =
            FileReceiver::create(dir.path(), Uuid::new_v4(), "report.pdf", bytes.len() as u64)
                .await
                .expect("receiver");
        receiver.write_chunk(0, &bytes).await.expect("chunk");

        // Removing the temporary makes the rename fail the way a full disk or
        // a revoked permission would: after the name has been reserved.
        let temp = receiver.temp_path().to_path_buf();
        tokio::fs::remove_file(&temp)
            .await
            .expect("unlink the temp");

        let err = receiver
            .finish(&digest(&bytes))
            .await
            .expect_err("there is nothing left to rename");
        assert!(matches!(err, SessionError::Io { .. }), "unexpected: {err}");
        assert!(
            !dir.path().join("report.pdf").exists(),
            "an empty file was published under the real name"
        );

        let mut entries = tokio::fs::read_dir(dir.path()).await.expect("list");
        assert!(
            entries.next_entry().await.expect("entry").is_none(),
            "the download directory should be empty"
        );
    }

    #[tokio::test]
    async fn an_out_of_order_chunk_is_refused_rather_than_written() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut receiver = FileReceiver::create(dir.path(), Uuid::new_v4(), "x.bin", 100)
            .await
            .expect("receiver");
        receiver.write_chunk(0, b"aaa").await.expect("first chunk");

        let err = receiver
            .write_chunk(2, b"bbb")
            .await
            .expect_err("chunk 1 never arrived");
        assert!(
            matches!(
                err,
                SessionError::ChunkOutOfOrder {
                    expected: 1,
                    got: 2,
                    ..
                }
            ),
            "unexpected: {err}"
        );
        assert_eq!(receiver.received(), 3, "the refused chunk must not count");
    }

    #[tokio::test]
    async fn a_replayed_chunk_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut receiver = FileReceiver::create(dir.path(), Uuid::new_v4(), "x.bin", 100)
            .await
            .expect("receiver");
        receiver.write_chunk(0, b"aaa").await.expect("first chunk");

        let err = receiver
            .write_chunk(0, b"aaa")
            .await
            .expect_err("chunk 0 already arrived");
        assert!(
            matches!(
                err,
                SessionError::ChunkOutOfOrder {
                    expected: 1,
                    got: 0,
                    ..
                }
            ),
            "unexpected: {err}"
        );
        assert_eq!(receiver.received(), 3);
    }

    #[tokio::test]
    async fn more_bytes_than_announced_are_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut receiver = FileReceiver::create(dir.path(), Uuid::new_v4(), "x.bin", 4)
            .await
            .expect("receiver");
        let err = receiver
            .write_chunk(0, b"aaaaaaa")
            .await
            .expect_err("seven bytes into a four-byte file");
        assert!(
            matches!(
                err,
                SessionError::TransferOversized {
                    received: 7,
                    declared: 4,
                    ..
                }
            ),
            "unexpected: {err}"
        );
    }

    #[tokio::test]
    async fn a_traversal_filename_cannot_escape_the_download_directory() {
        let root = tempfile::tempdir().expect("temp dir");
        let downloads = root.path().join("downloads");
        let bytes = b"owned".to_vec();

        let mut receiver = FileReceiver::create(
            &downloads,
            Uuid::new_v4(),
            "../../etc/passwd",
            bytes.len() as u64,
        )
        .await
        .expect("receiver");
        receiver.write_chunk(0, &bytes).await.expect("chunk");
        let path = receiver.finish(&digest(&bytes)).await.expect("verified");

        assert_eq!(path, downloads.join("passwd"));
        assert_eq!(path.parent(), Some(downloads.as_path()));
        assert!(!root.path().join("etc").exists());
    }

    #[tokio::test]
    async fn an_existing_file_is_never_clobbered() {
        let dir = tempfile::tempdir().expect("temp dir");
        write_temp_file(dir.path(), "report.pdf", b"the original").await;
        let bytes = b"the newcomer".to_vec();

        let mut receiver =
            FileReceiver::create(dir.path(), Uuid::new_v4(), "report.pdf", bytes.len() as u64)
                .await
                .expect("receiver");
        receiver.write_chunk(0, &bytes).await.expect("chunk");
        let path = receiver.finish(&digest(&bytes)).await.expect("verified");

        assert_eq!(path, dir.path().join("report (2).pdf"));
        assert_eq!(
            tokio::fs::read(dir.path().join("report.pdf"))
                .await
                .expect("original"),
            b"the original"
        );
        assert_eq!(tokio::fs::read(&path).await.expect("new file"), bytes);
    }

    #[tokio::test]
    async fn abandoning_a_transfer_leaves_no_partial_file_behind() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut receiver = FileReceiver::create(dir.path(), Uuid::new_v4(), "big.bin", 1_000_000)
            .await
            .expect("receiver");
        receiver
            .write_chunk(0, &body(BULK_CHUNK_BYTES))
            .await
            .expect("chunk");
        let temp = receiver.temp_path().to_path_buf();
        assert!(temp.exists());

        receiver.discard().await;

        assert!(!temp.exists());
        let mut entries = tokio::fs::read_dir(dir.path()).await.expect("list");
        assert!(
            entries.next_entry().await.expect("entry").is_none(),
            "the download directory should be empty"
        );
    }

    #[tokio::test]
    async fn the_sender_chunks_a_file_that_is_not_a_whole_number_of_chunks() {
        let dir = tempfile::tempdir().expect("temp dir");
        let bytes = body(2 * BULK_CHUNK_BYTES + 123);
        let path = write_temp_file(dir.path(), "payload.bin", &bytes).await;

        let sender = FileSender::open(&path).await.expect("sender");
        assert_eq!(sender.size(), bytes.len() as u64);

        let sink = RecordingSink::default();
        let mut seen = vec![];
        let computed = sender
            .stream(&sink, &mut |progress| seen.push(progress))
            .await
            .expect("streamed");

        let chunks = sink.chunks.lock();
        assert_eq!(chunks.len(), 3, "two full chunks and a remainder");
        assert_eq!(chunks[0].1.len(), BULK_CHUNK_BYTES);
        assert_eq!(chunks[1].1.len(), BULK_CHUNK_BYTES);
        assert_eq!(chunks[2].1.len(), 123);
        assert_eq!(
            chunks.iter().map(|(seq, _)| *seq).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(computed, digest(&bytes));
        assert_eq!(
            seen.last().map(|p| p.state.clone()),
            Some(TransferState::Complete)
        );
        assert_eq!(seen.last().map(|p| p.transferred), Some(bytes.len() as u64));
    }

    #[tokio::test]
    async fn an_empty_file_sends_no_chunks_and_still_verifies() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = write_temp_file(dir.path(), "empty.bin", b"").await;

        let sender = FileSender::open(&path).await.expect("sender");
        let sink = RecordingSink::default();
        let computed = sender.stream(&sink, &mut |_| {}).await.expect("streamed");

        assert!(sink.chunks.lock().is_empty());
        assert_eq!(computed, digest(b""));
    }

    /// The pair has to agree, or every transfer would fail verification.
    #[tokio::test]
    async fn what_the_sender_hashes_is_what_the_receiver_hashes() {
        let source = tempfile::tempdir().expect("temp dir");
        let downloads = tempfile::tempdir().expect("temp dir");
        let bytes = body(5 * BULK_CHUNK_BYTES + 9);
        let path = write_temp_file(source.path(), "movie.bin", &bytes).await;

        let sender = FileSender::open(&path).await.expect("sender");
        let sink = RecordingSink::default();
        let sent_digest = sender.stream(&sink, &mut |_| {}).await.expect("streamed");

        let mut receiver = FileReceiver::create(
            downloads.path(),
            sender.transfer_id(),
            sender.name(),
            sender.size(),
        )
        .await
        .expect("receiver");
        let chunks = sink.chunks.lock().clone();
        for (seq, payload) in &chunks {
            receiver.write_chunk(*seq, payload).await.expect("chunk");
        }
        let landed = receiver.finish(&sent_digest).await.expect("verified");

        assert_eq!(tokio::fs::read(&landed).await.expect("read back"), bytes);
    }

    #[tokio::test]
    async fn the_sender_waits_while_the_send_buffer_is_over_the_high_water_mark() {
        let dir = tempfile::tempdir().expect("temp dir");
        let bytes = body(BULK_CHUNK_BYTES);
        let path = write_temp_file(dir.path(), "paced.bin", &bytes).await;

        let sender = FileSender::open(&path).await.expect("sender");
        let sink = RecordingSink::default();
        // Three congested polls, then the buffer "drains".
        sink.congested_polls.store(3, Ordering::Release);

        let started = std::time::Instant::now();
        sender.stream(&sink, &mut |_| {}).await.expect("streamed");

        assert_eq!(sink.chunks.lock().len(), 1);
        assert!(
            started.elapsed() >= DRAIN_POLL,
            "the sender did not wait for the buffer to drain"
        );
        assert_eq!(
            sink.congested_polls.load(Ordering::Acquire),
            0,
            "the sender stopped polling before the buffer drained"
        );
    }

    #[tokio::test]
    async fn cancelling_mid_file_stops_the_stream_and_reports_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let bytes = body(8 * BULK_CHUNK_BYTES);
        let path = write_temp_file(dir.path(), "aborted.bin", &bytes).await;

        let sender = FileSender::open(&path).await.expect("sender");
        let cancel = sender.cancel_token();
        let sink = RecordingSink::default();

        let mut states = vec![];
        let err = sender
            .stream(&sink, &mut |progress| {
                // Pull the plug after the second chunk.
                if progress.transferred >= 2 * BULK_CHUNK_BYTES as u64 {
                    cancel.cancel();
                }
                states.push(progress.state);
            })
            .await
            .expect_err("cancelled");

        assert!(
            matches!(err, SessionError::TransferCancelled { .. }),
            "unexpected: {err}"
        );
        assert_eq!(
            sink.chunks.lock().len(),
            2,
            "sending continued after cancel"
        );
        assert_eq!(states.last(), Some(&TransferState::Cancelled));
    }

    #[tokio::test]
    async fn a_directory_cannot_be_sent_as_a_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let err = FileSender::open(dir.path()).await.expect_err("not a file");
        assert!(matches!(err, SessionError::Io { .. }), "unexpected: {err}");
    }

    #[test]
    fn a_colliding_name_keeps_its_extension() {
        assert_eq!(disambiguate("report.pdf", 2), "report (2).pdf");
        assert_eq!(disambiguate("archive.tar.gz", 3), "archive.tar (3).gz");
        assert_eq!(disambiguate("README", 2), "README (2)");
    }
}
