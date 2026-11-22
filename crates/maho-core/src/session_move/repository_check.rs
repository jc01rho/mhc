use super::bundle::RepositoryIdentity;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveRefusedError {
    pub target_cwd: String,
    pub source_cwd: Option<String>,
}

impl MoveRefusedError {
    pub const CODE: &'static str = "move_repository_mismatch";

    pub fn new(target_cwd: impl Into<String>, source_cwd: Option<String>) -> Self {
        Self {
            target_cwd: target_cwd.into(),
            source_cwd,
        }
    }
}

impl std::fmt::Display for MoveRefusedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.source_cwd {
            Some(source) => write!(
                f,
                "{}: {} is not the same repository the session was recorded in ({source}). Pick a checkout of the same repository, or copy the session into this folder instead.",
                Self::CODE,
                self.target_cwd
            ),
            None => write!(
                f,
                "{}: {} is not the same repository the session was recorded in. Pick a checkout of the same repository, or copy the session into this folder instead.",
                Self::CODE,
                self.target_cwd
            ),
        }
    }
}

impl std::error::Error for MoveRefusedError {}

pub fn same_repository(a: Option<&RepositoryIdentity>, b: Option<&RepositoryIdentity>) -> bool {
    let (Some(a), Some(b)) = (a, b) else {
        return false;
    };
    if !a.root_commits.is_empty() && !b.root_commits.is_empty() {
        return a
            .root_commits
            .iter()
            .any(|root| b.root_commits.contains(root));
    }
    matches!((&a.origin_url, &b.origin_url), (Some(a), Some(b)) if a == b)
}

pub async fn read_repository_identity(dir: &Path) -> Result<Option<RepositoryIdentity>, String> {
    async fn git(dir: &Path, args: &[&str]) -> Result<Option<String>, String> {
        let mut command = tokio::process::Command::new("git");
        command.arg("-C").arg(dir).args(args).kill_on_drop(true);
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
        ] {
            command.env_remove(key);
        }
        match tokio::time::timeout(std::time::Duration::from_secs(5), command.output()).await {
            Ok(Ok(output)) if output.status.success() => {
                Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
            }
            Ok(Ok(_)) | Err(_) => Ok(None),
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Ok(Err(e)) => Err(e.to_string()),
        }
    }
    if !tokio::fs::metadata(dir).await.is_ok_and(|m| m.is_dir()) {
        return Ok(None);
    }
    let roots = git(dir, &["rev-list", "--max-parents=0", "HEAD"])
        .await?
        .unwrap_or_default();
    let origin = git(dir, &["config", "--get", "remote.origin.url"]).await?;
    let mut root_commits: Vec<String> = roots
        .split_whitespace()
        .filter(|root| {
            (40..=64).contains(&root.len())
                && root
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        })
        .map(str::to_owned)
        .collect();
    root_commits.sort();
    let origin_url = origin
        .filter(|url| !url.trim().is_empty())
        .map(|url| normalize_remote_url(&url));
    Ok(
        (!root_commits.is_empty() || origin_url.is_some()).then_some(RepositoryIdentity {
            root_commits,
            origin_url,
        }),
    )
}

pub fn normalize_remote_url(url: &str) -> String {
    let trimmed = url.trim();
    let mut value = if !trimmed.contains("://") {
        let bare = trimmed.rsplit_once('@').map_or(trimmed, |(_, rest)| rest);
        bare.split_once(':')
            .filter(|(_, path)| !path.starts_with('/'))
            .map_or_else(|| bare.to_owned(), |(host, path)| format!("{host}/{path}"))
    } else {
        let bare = trimmed.split_once("://").map_or(trimmed, |(_, rest)| rest);
        let bare = bare.rsplit_once('@').map_or(bare, |(_, rest)| rest);
        let (host, path) = bare.split_once('/').unwrap_or((bare, ""));
        let host = host
            .split_once(':')
            .filter(|(_, port)| port.bytes().all(|c| c.is_ascii_digit()))
            .map_or(host, |(host, _)| host);
        format!("{host}/{path}")
    }
    .trim_end_matches('/')
    .to_lowercase();
    if value.ends_with(".git") {
        value.truncate(value.len() - 4);
    }
    value
}

pub async fn is_checkout_of_repository(
    cwd: &Path,
    repository: Option<&RepositoryIdentity>,
    read_override: Option<super::import::RepositoryIdentityReader<'_>>,
) -> Result<bool, String> {
    let resolved = super::session_lock::real_session_path_async(cwd).await;
    let target_identity = match read_override {
        Some(read) => read(&resolved)?,
        None => read_repository_identity(&resolved).await?,
    };
    Ok(same_repository(repository, target_identity.as_ref()))
}
