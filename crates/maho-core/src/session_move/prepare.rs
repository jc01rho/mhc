use super::{
    bundle::RepositoryIdentity,
    move_ledger::{MoveLedger, MoveRecord},
    repository_check::{MoveRefusedError, is_checkout_of_repository},
};
use std::path::Path;

pub struct PrepareMoveOptions<'a> {
    pub ledger: &'a MoveLedger,
    pub move_id: &'a str,
    pub cwd: Option<&'a Path>,
    pub repository: Option<&'a RepositoryIdentity>,
}

pub async fn prepare_move(options: PrepareMoveOptions<'_>) -> Result<MoveRecord, String> {
    let Some(cwd) = options.cwd else {
        return options
            .ledger
            .prepare(options.move_id)
            .await
            .map_err(|e| e.to_string());
    };
    if !is_checkout_of_repository(cwd, options.repository, None).await? {
        return Err(MoveRefusedError::new(cwd.to_string_lossy(), None).to_string());
    }
    options
        .ledger
        .prepare_for(options.move_id, Some(cwd))
        .await
        .map_err(|e| e.to_string())
}
