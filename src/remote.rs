//! The remote: a thin **synchronous** wrapper around an OpenDAL `Operator`.
//!
//! Design choice: stowe's core is synchronous (it's a CPU/disk-bound batch
//! tool, not a server). OpenDAL is async, so tokio is *quarantined here* — the
//! rest of the program never sees a `.await`. Concurrency, where it actually
//! pays off (uploading many objects), is done inside [`Remote::put_files`] via
//! a bounded `buffer_unordered` over the runtime.
//!
//! OpenDAL itself is the pluggable-backend layer: `local:` builds an `Fs`
//! operator, `s3:`/`b2:` an `S3` operator — same `Operator`, same four ops.

use anyhow::{Context, Result, bail};
use futures::stream::{self, StreamExt};
use opendal::{Operator, services};
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;

/// How many object uploads to keep in flight at once.
const UPLOAD_CONCURRENCY: usize = 8;
/// Streaming chunk size, so we never load a whole (possibly huge) file in RAM.
const CHUNK: usize = 1 << 20; // 1 MiB

/// Map a content hash to its object key: `objects/<first2>/<rest>`.
pub fn object_key(hash: &str) -> String {
    format!("objects/{}/{}", &hash[..2], &hash[2..])
}

/// A remote store. Holds its own tokio runtime and an OpenDAL operator.
pub struct Remote {
    op: Operator,
    rt: tokio::runtime::Runtime,
}

/// Build a [`Remote`] from a remote URL.
///
/// Supported schemes:
/// - `local:<path>` (or a bare path) — a folder / mounted drive / NAS
/// - `s3://<bucket>[/<root>]` — any S3-compatible store (AWS, Backblaze B2, …);
///   credentials come from the standard `AWS_*` environment variables, and an
///   `AWS_ENDPOINT_URL` lets you point at B2/MinIO/etc.
pub fn open(url: &str) -> Result<Remote> {
    let op = build_operator(url)?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    Ok(Remote { op, rt })
}

fn build_operator(url: &str) -> Result<Operator> {
    // local:<path>  (or a bare path)
    let local_path = url.strip_prefix("local:").unwrap_or(url);
    let is_s3 = url.starts_with("s3://");

    if is_s3 {
        let rest = url.trim_start_matches("s3://");
        let (bucket, root) = rest.split_once('/').unwrap_or((rest, ""));
        if bucket.is_empty() {
            bail!("s3 remote needs a bucket: s3://<bucket>[/<root>]");
        }
        let mut b = services::S3::default().bucket(bucket);
        if !root.is_empty() {
            b = b.root(root);
        }
        // Region + endpoint from env if present (endpoint is how you target B2).
        let region = std::env::var("AWS_REGION").unwrap_or_else(|_| "auto".into());
        b = b.region(&region);
        if let Ok(ep) = std::env::var("AWS_ENDPOINT_URL") {
            b = b.endpoint(&ep);
        }
        // Access keys are read from AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY
        // by OpenDAL's default credential loader.
        return Ok(Operator::new(b)?.finish());
    }

    // Filesystem backend. Make sure the root exists so first push works.
    std::fs::create_dir_all(local_path)
        .with_context(|| format!("creating remote dir {local_path}"))?;
    Ok(Operator::new(services::Fs::default().root(local_path))?.finish())
}

impl Remote {
    pub fn exists(&self, key: &str) -> Result<bool> {
        self.rt.block_on(async { Ok(self.op.exists(key).await?) })
    }

    pub fn put_bytes(&self, key: &str, data: &[u8]) -> Result<()> {
        self.rt.block_on(async {
            self.op.write(key, data.to_vec()).await?;
            Ok(())
        })
    }

    pub fn get_bytes(&self, key: &str) -> Result<Vec<u8>> {
        self.rt
            .block_on(async { Ok(self.op.read(key).await?.to_vec()) })
    }

    /// Download `key` to a local path (creating parent dirs).
    pub fn get_file(&self, key: &str, dest: &Path) -> Result<()> {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = self.get_bytes(key)?;
        std::fs::write(dest, bytes)?;
        Ok(())
    }

    /// Upload many `(key, source-file)` pairs, skipping keys already present.
    /// Runs up to [`UPLOAD_CONCURRENCY`] uploads at once. Returns how many were
    /// actually uploaded (i.e. weren't already there — that's the dedup skip).
    pub fn put_files(&self, items: Vec<(String, PathBuf)>) -> Result<usize> {
        self.rt.block_on(async {
            let op = &self.op;
            let results: Vec<Result<usize>> = stream::iter(items)
                .map(|(key, src)| async move {
                    if op.exists(&key).await? {
                        Ok(0usize)
                    } else {
                        upload_one(op, &key, &src).await?;
                        Ok(1usize)
                    }
                })
                .buffer_unordered(UPLOAD_CONCURRENCY)
                .collect()
                .await;

            let mut uploaded = 0;
            for r in results {
                uploaded += r?;
            }
            Ok(uploaded)
        })
    }
}

/// Stream a local file into the remote under `key`, chunk by chunk.
async fn upload_one(op: &Operator, key: &str, src: &Path) -> Result<()> {
    let mut file = tokio::fs::File::open(src)
        .await
        .with_context(|| format!("opening {}", src.display()))?;
    let mut writer = op.writer(key).await?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        writer.write(buf[..n].to_vec()).await?;
    }
    writer.close().await?;
    Ok(())
}
