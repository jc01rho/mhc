pub use memory_core::reflection::run_ledger::{
    ReservationLedgerError, ReservationRunLedger, parse_reservation_run_ledger,
};

pub fn worktree_from_ledger(
    repo_path: &std::path::Path,
    identity: &str,
    ledger: &ReservationRunLedger,
) -> Result<memory_core::reflection::worktree::ReflectionWorktree, memory_core::git::errors::GitError>
{
    let value = ledger.value();
    let string = |key: &str| {
        value[key]
            .as_str()
            .expect("validated ledger string")
            .to_owned()
    };
    Ok(memory_core::reflection::worktree::ReflectionWorktree {
        parent: memory_core::git::GitMemoryRepo::open(repo_path, identity)?,
        dir: string("worktreeDir").into(),
        branch: string("worktreeBranch"),
        base_commit_sha: string("baseSha"),
        git_file_path: string("gitFilePath").into(),
        git_file_snapshot: string("gitFileSnapshot"),
        common_config_path: string("commonConfigPath").into(),
        common_config_snapshot: value["commonConfigSnapshot"].as_str().map(str::to_owned),
        exec: memory_core::git::exec::create_git_exec(Default::default()),
    })
}
