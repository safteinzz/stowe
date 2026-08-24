//! The remote: a thin **synchronous** wrapper around an OpenDAL `Operator`.
//!
//! Design choice: stowe's core is synchronous (it's a CPU/disk-bound batch
//! tool, not a server). OpenDAL is async, so tokio is *quarantined here* - the
//! rest of the program never sees a `.await`. Concurrency, where it actually
//! pays off (uploading many objects), is done inside [`Remote::put_files`] via
//! a bounded `buffer_unordered` over the runtime.
//!
//! OpenDAL itself is the pluggable-backend layer: `local:` builds an `Fs`
//! operator, `s3:`/`b2:` an `S3` operator - same `Operator`, same four ops.

use anyhow::{Context, Result, bail};
use futures::stream::{self, StreamExt};
use opendal::{Operator, services};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use crate::repo::Repo;
use anyhow::anyhow;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
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
/// - `local:<path>` (or a bare path) - a folder / mounted drive / NAS
/// - `s3://<bucket>[/<root>]` - any S3-compatible store (AWS, Backblaze B2, …);
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
    // Write to a temp file and rename into place on close, so a killed push
    // (Ctrl+C, crash, unplugged drive) never leaves a corrupt file sitting
    // under its final content-hash key - which future pushes would then
    // mistake for a complete, valid object and skip forever.
    let tmp_dir = Path::new(local_path).join(".stowe-tmp");
    std::fs::create_dir_all(&tmp_dir)?;
    Ok(Operator::new(
        services::Fs::default()
            .root(local_path)
            .atomic_write_dir(&tmp_dir.to_string_lossy()),
    )?
    .finish())
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
    /// actually uploaded (i.e. weren't already there - that's the dedup skip).
    pub fn put_files(&self, items: Vec<(String, PathBuf)>) -> Result<usize> {
        let total = items.len();
        let done = Arc::new(AtomicUsize::new(0));
        // Live progress on stderr, terminal-only - so `stowe push` never looks
        // hung on a big upload, while piped/redirected output stays clean.
        let show = std::io::stderr().is_terminal();
        self.rt.block_on(async {
            let op = &self.op;
            let done = &done;
            let results: Vec<Result<usize>> = stream::iter(items)
                .map(|(key, src)| async move {
                    let r = if op.exists(&key).await? {
                        Ok(0usize)
                    } else {
                        upload_one(op, &key, &src).await?;
                        Ok(1usize)
                    };
                    let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if show {
                        eprint!("\r\x1b[Kpushing... {n}/{total}");
                        let _ = std::io::stderr().flush();
                    }
                    r
                })
                .buffer_unordered(UPLOAD_CONCURRENCY)
                .collect()
                .await;

            if show && total > 0 {
                eprint!("\r\x1b[K"); // wipe the progress line before the summary
                let _ = std::io::stderr().flush();
            }
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

/// The on-disk format for a remote: an explicit config override, or the scheme
/// default (local paths are playable mirrors, everything else is an object store).
pub(crate) fn remote_format(
    cfg: &crate::model::Config,
    name: &str,
    url: &str,
) -> crate::mirror::Format {
    match cfg.formats.get(name).map(String::as_str) {
        Some("backup") => crate::mirror::Format::Backup,
        Some("mirror") => crate::mirror::Format::Mirror,
        _ if crate::mirror::local_root(url).is_some() => crate::mirror::Format::Mirror,
        _ => crate::mirror::Format::Backup,
    }
}

pub(crate) fn remote_url(repo: &Repo, name: &str) -> Result<String> {
    repo.config()?
        .remotes
        .get(name)
        .cloned()
        .ok_or_else(|| anyhow!("no remote named `{name}` - add one: stowe remote add {name} <url>"))
}

/// Whether a remote's location is usable right now. A local path is reachable
/// if it exists, or its parent does (so a first push can still create it).
/// Non-local remotes (s3) are assumed reachable; their backend handles it.
pub(crate) fn remote_reachable(url: &str) -> bool {
    match crate::mirror::local_root(url) {
        Some(root) => root.exists() || root.parent().map(Path::exists).unwrap_or(false),
        None => true,
    }
}

/// Make sure a remote is really there before we write a byte to it.
///
/// The hard lesson behind this: *"a folder exists"* is not proof a remote is
/// mounted. Unmounting never removes the mountpoint, so a leftover empty
/// directory will happily accept an entire library onto the local disk. So:
///
/// 1. If the remote has a `mount` command, that command is the authority (it
///    can ask the kernel; a stale folder can't fool it). Always run it, every
///    time. It's expected to be a no-op when already mounted.
/// 2. Belt and braces: a genuinely mounted remote lives on its own filesystem.
///    If it's still on the local disk after mounting "succeeded", refuse.
/// 3. With no mount command: if we've pushed here before, the remote must still
///    carry its marker. Gone means the drive is gone, and we must never
///    recreate the folder.
pub(crate) fn ensure_reachable(
    repo: &Repo,
    cfg: &crate::model::Config,
    name: &str,
    url: &str,
) -> Result<()> {
    // Non-local remotes (s3) have no path to mount; their backend handles it.
    let Some(root) = crate::mirror::local_root(url) else {
        return Ok(());
    };

    // The remote knows how to make itself available: let it, before we judge.
    if let Some(cmd) = cfg.mounts.get(name) {
        run_mount(name, cmd)?;
        // A mount command exists precisely because this remote lives on its own
        // device. If we'd still be writing to the local disk, the mount didn't
        // take, whatever the script claimed.
        if on_local_disk(&root) {
            bail!(
                "`{name}`: the mount command succeeded, but {} is still on your local disk. \
                 Refusing to write there - the drive would be backed up to the wrong place.",
                crate::paths::short(&root)
            );
        }
    }

    // Written here before? Then the remote must still carry its marker. If it's
    // gone, so is the drive, and we must never recreate the folder: that is
    // exactly how an entire library ends up copied onto the local disk.
    let known = repo.remote_head(name)?.is_some();
    if known && crate::mirror::detect_format(&root) == crate::mirror::Format::Empty {
        bail!(
            "remote `{name}` ({}) has been pushed to before, but isn't there now. \
             Is the drive connected? (refusing to recreate it)",
            crate::paths::short(&root)
        );
    }

    if !remote_reachable(url) {
        bail!(
            "remote `{name}` ({}) isn't reachable. Is the drive connected?",
            crate::paths::short(&root)
        );
    }
    Ok(())
}

/// True when `path` sits on the same filesystem as `/`, i.e. nothing is really
/// mounted there. Any genuine mount (drive, phone, sshfs) gets its own device
/// id, so this catches "the script said OK but we'd be writing to local disk".
/// Falls back to the nearest existing ancestor, since the remote's own folder
/// may not exist yet on a first push.
///
/// Best-effort, and deliberately secondary to the marker check: it only sees the
/// mistake when the remote path lives on the root filesystem, so a separate
/// `/home` partition (or a path under `/tmp`) hides it. The marker check is what
/// actually guarantees we never rewrite a remote that isn't there.
#[cfg(unix)]
pub(crate) fn on_local_disk(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let device_of = |p: &Path| -> Option<u64> {
        let mut cur = Some(p);
        while let Some(c) = cur {
            if let Ok(md) = std::fs::metadata(c) {
                return Some(md.dev());
            }
            cur = c.parent();
        }
        None
    };
    match (device_of(path), device_of(Path::new("/"))) {
        (Some(here), Some(root_fs)) => here == root_fs,
        _ => false, // can't tell: don't block on a guess
    }
}

#[cfg(not(unix))]
pub(crate) fn on_local_disk(_path: &Path) -> bool {
    false
}

/// Run a remote's mount command through the platform shell, so the configured
/// value can be an inline command *or* a path to a script (a script path is
/// just a command). Echoed before running: it's your shell, but you should see
/// what stowe is about to execute.
pub(crate) fn run_mount(name: &str, cmd: &str) -> Result<()> {
    use colored::Colorize;
    println!(
        "{} {}",
        "mounting".dimmed(),
        format!("`{name}`: {cmd}").dimmed()
    );

    #[cfg(windows)]
    let status = std::process::Command::new("cmd").args(["/C", cmd]).status();
    #[cfg(not(windows))]
    let status = std::process::Command::new("sh").arg("-c").arg(cmd).status();

    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => bail!(
            "mount command for `{name}` failed (exit {})",
            s.code().unwrap_or(1)
        ),
        Err(e) => bail!("could not run the mount command for `{name}`: {e}"),
    }
}
