//! The local implementation of the file operations (senpi `files/local-files.ts`). Every operation resolves
//! its path against the scope root, checks containment through [`check_path`] before any effect, and runs
//! inside [`gated`].

use std::fs::Metadata;
use std::future::Future;
use std::io::{self, SeekFrom};
use std::path::{Path, PathBuf};

use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::errors::{MachineOpError, OpErrorCode};
use crate::files::glob::local_glob;
use crate::files::grep::local_grep;
use crate::files::path_lock::acquire_file_mutation_lock;
use crate::node_path::{is_absolute_pattern, resolve};
use crate::policy::filesystem::FilesystemOperation;
use crate::policy::gate::{check_path, gated, resolve_op_path};
use crate::types::{
    AccessMode, EditRequest, EditResult, EntryType, GlobRequest, GlobResult, GrepRequest,
    GrepResult, ListEntry, ListRequest, ListResult, OpArgs, OpContext, OpName, PathLock,
    ReadRequest, ReadResult, StatRequest, StatResult, WriteRequest, WriteResult,
};

const GLOB_SYNTAX: [char; 6] = ['*', '?', '[', ']', '{', '}'];

fn entry_type_of(kind: std::fs::FileType) -> EntryType {
    if kind.is_file() {
        EntryType::File
    } else if kind.is_dir() {
        EntryType::Directory
    } else if kind.is_symlink() {
        EntryType::Symlink
    } else {
        EntryType::Other
    }
}

fn read_limit_error(path: &Path, size: u64, limit: u64) -> MachineOpError {
    MachineOpError::new(
        OpErrorCode::LimitExceeded,
        format!(
            "{} is {size} bytes, above the {limit}-byte read limit",
            path.display()
        ),
    )
}

/// Per-canonical-path ordering for mutations, unless the caller already holds the slot.
async fn with_path_lock<T>(
    path: &Path,
    ctx: &OpContext,
    effect: impl Future<Output = Result<T, MachineOpError>>,
) -> Result<T, MachineOpError> {
    if ctx.path_lock == Some(PathLock::Caller) {
        return effect.await;
    }
    let _slot = acquire_file_mutation_lock(path, &ctx.signal).await?;
    effect.await
}

async fn read_within_limit(path: &Path, limit: u64) -> Result<Vec<u8>, MachineOpError> {
    // An unbounded limit (the tool path) reads exactly as `fs::read` always has, with no extra stat.
    if limit != u64::MAX {
        let info = tokio::fs::metadata(path).await?;
        if info.len() > limit {
            return Err(read_limit_error(path, info.len(), limit));
        }
    }
    Ok(tokio::fs::read(path).await?)
}

async fn read_range(
    path: &Path,
    req: &ReadRequest,
    limit: u64,
) -> Result<ReadResult, MachineOpError> {
    let mut file = tokio::fs::File::open(path).await?;
    let size = file.metadata().await?.len();
    let offset = req.offset.unwrap_or(0);
    let available = size.saturating_sub(offset);
    if req.limit_bytes.is_none() && available > limit {
        return Err(read_limit_error(path, available, limit));
    }
    let length = req
        .limit_bytes
        .map_or(available, |wanted| wanted.min(available));
    file.seek(SeekFrom::Start(offset)).await?;
    let mut bytes = Vec::new();
    file.take(length).read_to_end(&mut bytes).await?;
    Ok(ReadResult {
        path: path.to_path_buf(),
        bytes,
        size,
        offset,
    })
}

async fn write_range(path: &Path, req: &WriteRequest, offset: u64) -> Result<(), MachineOpError> {
    let length = req.bytes.len() as u64;
    if req.total.is_some_and(|total| total < offset + length) {
        return Err(MachineOpError::new(
            OpErrorCode::InvalidArgument,
            "total must cover offset + bytes",
        ));
    }
    // A ranged write keeps the bytes outside its range: the file is created when missing but never truncated.
    let mut file = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .await?;
    file.seek(SeekFrom::Start(offset)).await?;
    file.write_all(&req.bytes).await?;
    file.flush().await?;
    if let Some(total) = req.total {
        file.set_len(total).await?;
    }
    Ok(())
}

/// `access(2)` is approximated by opening: a file is opened for the requested mode without creating or
/// truncating it, a directory is listed for read and judged by its read-only flag for write.
async fn check_access(path: &Path, mode: AccessMode, info: &Metadata) -> io::Result<()> {
    let wants_read = matches!(mode, AccessMode::Read | AccessMode::ReadWrite);
    let wants_write = matches!(mode, AccessMode::Write | AccessMode::ReadWrite);
    if info.is_dir() {
        if wants_read {
            // Listing it is the read check; a directory this process may not read fails here.
            let _ = tokio::fs::read_dir(path).await?;
        }
        if wants_write && info.permissions().readonly() {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied));
        }
        return Ok(());
    }
    tokio::fs::OpenOptions::new()
        .read(wants_read)
        .write(wants_write)
        .open(path)
        .await?;
    Ok(())
}

#[cfg(unix)]
fn mode_of(info: &Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    info.mode()
}

#[cfg(not(unix))]
fn mode_of(info: &Metadata) -> u32 {
    let type_bits = if info.is_dir() { 0o040_000 } else { 0o100_000 };
    let permission_bits = if info.permissions().readonly() {
        0o444
    } else {
        0o666
    };
    type_bits | permission_bits
}

fn mtime_ms(info: &Metadata) -> f64 {
    match info.modified() {
        Ok(modified) => match modified.duration_since(std::time::UNIX_EPOCH) {
            Ok(after) => after.as_secs_f64() * 1000.0,
            Err(before) => -(before.duration().as_secs_f64() * 1000.0),
        },
        Err(_) => 0.0,
    }
}

/// The local file operations. No state: ordering of one path's mutations lives in
/// [`crate::acquire_file_mutation_lock`], shared by every instance.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalFileOps;

/// Create the local file operations (senpi `createLocalFileOps`).
pub fn create_local_file_ops() -> LocalFileOps {
    LocalFileOps
}

impl LocalFileOps {
    pub async fn read(
        &self,
        req: &ReadRequest,
        ctx: &OpContext,
    ) -> Result<ReadResult, MachineOpError> {
        let path = resolve_op_path(&req.path, ctx);
        let limit = ctx.limits.read_max_bytes;
        if let Some(limit_bytes) = req.limit_bytes.filter(|wanted| *wanted > limit) {
            return Err(MachineOpError::new(
                OpErrorCode::LimitExceeded,
                format!("limit_bytes {limit_bytes} is above the {limit}-byte read limit"),
            ));
        }
        check_path(&path, FilesystemOperation::Read, OpName::FilesRead, ctx).await?;
        gated(OpName::FilesRead, OpArgs::Read(req), ctx, async {
            if req.offset.is_some() || req.limit_bytes.is_some() {
                return read_range(&path, req, limit).await;
            }
            let bytes = read_within_limit(&path, limit).await?;
            Ok(ReadResult {
                path: path.clone(),
                size: bytes.len() as u64,
                offset: 0,
                bytes,
            })
        })
        .await
    }

    pub async fn write(
        &self,
        req: &WriteRequest,
        ctx: &OpContext,
    ) -> Result<WriteResult, MachineOpError> {
        let path = resolve_op_path(&req.path, ctx);
        check_path(&path, FilesystemOperation::Write, OpName::FilesWrite, ctx).await?;
        with_path_lock(
            &path,
            ctx,
            gated(OpName::FilesWrite, OpArgs::Write(req), ctx, async {
                if req.create_parent_dirs
                    && let Some(parent) = path.parent()
                {
                    tokio::fs::create_dir_all(parent).await?;
                }
                match req.offset {
                    None => tokio::fs::write(&path, &req.bytes).await?,
                    Some(offset) => write_range(&path, req, offset).await?,
                }
                Ok(WriteResult {
                    path: path.clone(),
                    written_bytes: req.bytes.len() as u64,
                })
            }),
        )
        .await
    }

    pub async fn edit(
        &self,
        req: &EditRequest,
        ctx: &OpContext,
    ) -> Result<EditResult, MachineOpError> {
        let path = resolve_op_path(&req.path, ctx);
        if req.exact.is_empty() {
            return Err(MachineOpError::new(
                OpErrorCode::InvalidPattern,
                "exact substring must not be empty",
            ));
        }
        check_path(&path, FilesystemOperation::Write, OpName::FilesEdit, ctx).await?;
        with_path_lock(
            &path,
            ctx,
            gated(OpName::FilesEdit, OpArgs::Edit(req), ctx, async {
                let raw = read_within_limit(&path, ctx.limits.read_max_bytes).await?;
                let content = String::from_utf8_lossy(&raw);
                let replacements = content.matches(req.exact.as_str()).count();
                if replacements == 0 {
                    return Err(MachineOpError::new(
                        OpErrorCode::NotFound,
                        "exact substring was not found",
                    ));
                }
                let next = content.replace(req.exact.as_str(), &req.replacement);
                tokio::fs::write(&path, next.as_bytes()).await?;
                let write_result = WriteResult {
                    path: path.clone(),
                    written_bytes: next.len() as u64,
                };
                Ok(EditResult {
                    path: path.clone(),
                    content: next,
                    replacements,
                    write_result,
                })
            }),
        )
        .await
    }

    pub async fn glob(
        &self,
        req: &GlobRequest,
        ctx: &OpContext,
    ) -> Result<GlobResult, MachineOpError> {
        let mut cwd = resolve_op_path(req.cwd.as_deref().unwrap_or(Path::new(".")), ctx);
        let mut pattern = req.pattern.clone();
        let leaves_the_root = is_absolute_pattern(&pattern)
            || pattern.split(['/', '\\']).any(|segment| segment == "..");
        if ctx.scope.root.is_some() && leaves_the_root {
            // An absolute or ".." pattern is judged by where its fixed part resolves, like every other path:
            // the check below allows it inside the scope root and refuses it outside.
            let Some((base, rest)) = split_glob_base(&pattern, &cwd) else {
                return Err(MachineOpError::policy_denied(
                    "a glob pattern may not use '..' after a wildcard",
                ));
            };
            cwd = base;
            pattern = rest;
        }
        check_path(&cwd, FilesystemOperation::Enumerate, OpName::FilesGlob, ctx).await?;
        gated(
            OpName::FilesGlob,
            OpArgs::Glob(req),
            ctx,
            local_glob(&pattern, &cwd, req.limit, &ctx.signal),
        )
        .await
    }

    pub async fn grep(
        &self,
        req: &GrepRequest,
        ctx: &OpContext,
    ) -> Result<GrepResult, MachineOpError> {
        let path = resolve_op_path(&req.path, ctx);
        check_path(
            &path,
            FilesystemOperation::Enumerate,
            OpName::FilesGrep,
            ctx,
        )
        .await?;
        gated(
            OpName::FilesGrep,
            OpArgs::Grep(req),
            ctx,
            local_grep(
                &path,
                &req.pattern,
                req.limit,
                ctx.limits.read_max_bytes,
                &ctx.signal,
            ),
        )
        .await
    }

    pub async fn stat(
        &self,
        req: &StatRequest,
        ctx: &OpContext,
    ) -> Result<StatResult, MachineOpError> {
        let path = resolve_op_path(&req.path, ctx);
        check_path(&path, FilesystemOperation::Read, OpName::FilesStat, ctx).await?;
        gated(OpName::FilesStat, OpArgs::Stat(req), ctx, async {
            let info = tokio::fs::metadata(&path).await?;
            if let Some(mode) = req.access {
                check_access(&path, mode, &info).await?;
            }
            Ok(StatResult {
                path: path.clone(),
                entry_type: entry_type_of(info.file_type()),
                size: info.len(),
                mtime_ms: mtime_ms(&info),
                mode: mode_of(&info),
            })
        })
        .await
    }

    pub async fn list(
        &self,
        req: &ListRequest,
        ctx: &OpContext,
    ) -> Result<ListResult, MachineOpError> {
        let path = resolve_op_path(&req.path, ctx);
        check_path(
            &path,
            FilesystemOperation::Enumerate,
            OpName::FilesList,
            ctx,
        )
        .await?;
        gated(OpName::FilesList, OpArgs::List(req), ctx, async {
            let mut read_dir = tokio::fs::read_dir(&path).await?;
            let mut entries = Vec::new();
            while let Some(dirent) = read_dir.next_entry().await? {
                let kind = dirent.file_type().await?;
                entries.push(ListEntry {
                    name: dirent.file_name().to_string_lossy().into_owned(),
                    entry_type: entry_type_of(kind),
                });
            }
            Ok(ListResult {
                path: path.clone(),
                entries,
            })
        })
        .await
    }
}

fn glob_base_dir(absolute: bool, fixed: &[&str], cwd: &Path) -> PathBuf {
    let mut joined = fixed.join("/");
    if absolute && (joined.is_empty() || (cfg!(windows) && joined.ends_with(':'))) {
        joined.push('/');
    }
    resolve(cwd, Path::new(&joined))
}

/// Split a glob pattern into the directory its fixed leading segments name (resolved against `cwd`) and
/// the rest, which starts at the first segment with glob syntax. A pattern with no glob syntax splits
/// before its last segment; one ending in a separator lists that directory like `*`. `None` when `..`
/// follows a wildcard: where that lands is known only after matching.
fn split_glob_base(pattern: &str, cwd: &Path) -> Option<(PathBuf, String)> {
    let segments: Vec<&str> = pattern.split(['/', '\\']).collect();
    let first_glob = segments
        .iter()
        .position(|segment| segment.contains(GLOB_SYNTAX));
    let cut = first_glob.unwrap_or(segments.len() - 1);
    let (fixed, rest) = segments.split_at(cut);
    if rest.contains(&"..") {
        return None;
    }
    let base = glob_base_dir(is_absolute_pattern(pattern), fixed, cwd);
    let rest = rest.join("/");
    if rest.is_empty() {
        return Some((base, "*".to_owned()));
    }
    Some((base, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn an_absolute_pattern_splits_at_its_first_glob_segment() {
        let cwd = Path::new("/cwd");
        assert_eq!(
            split_glob_base("/r/dir/*.ts", cwd),
            Some((PathBuf::from("/r/dir"), "*.ts".to_owned()))
        );
        assert_eq!(
            split_glob_base("/*.ts", cwd),
            Some((PathBuf::from("/"), "*.ts".to_owned()))
        );
        assert_eq!(
            split_glob_base("/r/{a,b}/x/**/*.ts", cwd),
            Some((PathBuf::from("/r"), "{a,b}/x/**/*.ts".to_owned()))
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_dotdot_pattern_is_resolved_against_the_cwd() {
        let cwd = Path::new("/r/dir");
        assert_eq!(
            split_glob_base("../dir/*.ts", cwd),
            Some((PathBuf::from("/r/dir"), "*.ts".to_owned()))
        );
        assert_eq!(
            split_glob_base("../../*", cwd),
            Some((PathBuf::from("/"), "*".to_owned()))
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_pattern_ending_in_a_separator_lists_that_directory_like_star() {
        let cwd = Path::new("/r/dir");
        assert_eq!(
            split_glob_base("/r/dir/", cwd),
            Some((PathBuf::from("/r/dir"), "*".to_owned()))
        );
        assert_eq!(
            split_glob_base("../dir/", cwd),
            Some((PathBuf::from("/r/dir"), "*".to_owned()))
        );
        assert_eq!(
            split_glob_base("/", cwd),
            Some((PathBuf::from("/"), "*".to_owned()))
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_pattern_without_glob_syntax_splits_before_its_last_segment() {
        let cwd = Path::new("/r");
        assert_eq!(
            split_glob_base("/r/dir/a.ts", cwd),
            Some((PathBuf::from("/r/dir"), "a.ts".to_owned()))
        );
        assert_eq!(
            split_glob_base("dir/a.ts", cwd),
            Some((PathBuf::from("/r/dir"), "a.ts".to_owned()))
        );
        assert_eq!(
            split_glob_base("a.ts", cwd),
            Some((PathBuf::from("/r"), "a.ts".to_owned()))
        );
    }

    #[cfg(unix)]
    #[test]
    fn dotdot_after_a_wildcard_has_no_base() {
        let cwd = Path::new("/r");
        assert_eq!(split_glob_base("dir/*/../a.ts", cwd), None);
        assert_eq!(split_glob_base("/r/**/../a.ts", cwd), None);
        assert_eq!(split_glob_base("*/..", cwd), None);
    }

    #[cfg(unix)]
    #[test]
    fn relative_patterns_keep_their_wildcard_tail() {
        let cwd = Path::new("/r");
        assert_eq!(
            split_glob_base("src/**/*.rs", cwd),
            Some((PathBuf::from("/r/src"), "**/*.rs".to_owned()))
        );
    }
}
