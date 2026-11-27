//! `find_session` (senpi `modes/rpc/find-session.ts`, senpi#6429): which session FILE of THIS host carries a
//! durable id. A client that binds a thread to a session on another machine keeps the routing key and the durable
//! id, not the path; when that session is closed and unlisted (a move that was given back after its release leaves
//! the file writable and the session not open), the key names nothing and `open_session` has no path to take. This
//! answers the path and the folder its header names.
//!
//! It is a read: it opens, claims, unloads, seals and rewrites nothing, and touches no move record. It reads only
//! this host's own session folder, never a path from the request, which carries the durable id alone. It is
//! bounded: a host with more session files than [`FIND_SESSION_FILE_LIMIT`] answers a typed error instead of a
//! partial answer, and no file is read before that bound is decided.
//!
//! senpi names every session file it creates `<timestamp>_<durable id>.jsonl`, so the files named for the id are read
//! first and every other header only when none of them carries it (senpi#7136): on a host with ~10,000 session files
//! the full header pass took about a second, long enough to push a device's closed-session lookup past the account's
//! probe bound. A file created under a caller-chosen name (`--session <path>`) is still found by that full pass.
//!
//! ADAPTATION: senpi classes `find_session` as `device_admin` on a delegated connection (it answers a file's path
//! and folder, which is what makes a path-naming `open_session` `device_admin`). This host has no delegated
//! connections yet, so every connection that may send a host command may send this one.
use crate::session_path_key::canonical_session_path;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;

/// Session files this host may hold before one lookup refuses rather than walking them all.
pub const FIND_SESSION_FILE_LIMIT: usize = 20_000;
/// A session header is the file's first line; it carries the id and the folder and nothing else is read.
const HEADER_READ_BYTES: usize = 64 * 1024;
const READERS: usize = 16;

/// A host built with no agent directory at all keeps no session files to look in; every host this crate starts has
/// one.
pub const RPC_ERROR_FIND_SESSION_UNSUPPORTED: &str = "find_session_unsupported";
pub const RPC_ERROR_FIND_SESSION_TOO_MANY: &str = "find_session_too_many";
const RPC_ERROR_INVALID_ARGUMENT: &str = "invalid_argument";

/// More session files than one lookup may walk (senpi `FindSessionTooManyError`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TooMany {
    limit: usize,
}

impl TooMany {
    fn detail(self) -> String {
        format!(
            "This device keeps more than {} session files, so a lookup by durable id was not run. Open the session by its path instead.",
            self.limit
        )
    }
}

/// One session file that carries the durable id. `open` says an engine of this host holds the session now; it is
/// a hint, the id is what a caller checks.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    session_path: String,
    cwd: String,
    open: bool,
}

/// The file's first line, read whole or not at all; anything that is not a session header is not a candidate.
/// Returns the header's `id` and `cwd` (empty when the header names none).
async fn header_of(file: &Path) -> Option<(String, String)> {
    let handle = tokio::fs::File::open(file).await.ok()?;
    let mut limited = handle.take(HEADER_READ_BYTES as u64);
    let mut data = Vec::with_capacity(HEADER_READ_BYTES);
    limited.read_to_end(&mut data).await.ok()?;
    let newline = data.iter().position(|byte| *byte == b'\n');
    if newline.is_none() && data.len() == HEADER_READ_BYTES {
        return None;
    }
    let end = newline.unwrap_or(data.len());
    let header: Value = serde_json::from_slice(data.get(..end)?).ok()?;
    let header = header.as_object()?;
    if header.get("type").and_then(Value::as_str) != Some("session") {
        return None;
    }
    let id = header.get("id").and_then(Value::as_str)?;
    let cwd = header
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Some((id.to_owned(), cwd.to_owned()))
}

/// The `.jsonl` names directly in `folder`; a folder that cannot be listed lists nothing.
async fn jsonl_names(folder: &Path) -> Vec<std::ffi::OsString> {
    let Ok(mut names) = tokio::fs::read_dir(folder).await else {
        return Vec::new();
    };
    let mut listed = Vec::new();
    while let Ok(Some(entry)) = names.next_entry().await {
        let name = entry.file_name();
        if name.to_string_lossy().ends_with(".jsonl") {
            listed.push(name);
        }
    }
    listed
}

/// Every session file of this host, counted before any of them is opened: the files in each folder of `sessions/`
/// and the files directly in it, where a session started with that folder as its session dir keeps its file
/// (senpi#7017). One folder per cwd: a host with thousands of them lists them `READERS` at a time, not one after
/// another (senpi#7136).
async fn session_files(agent_dir: &Path, limit: usize) -> Result<Vec<PathBuf>, TooMany> {
    let root = agent_dir.join("sessions");
    let mut files = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(&root).await else {
        return Ok(files);
    };
    let mut folders = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let Ok(kind) = entry.file_type().await else {
            continue;
        };
        if kind.is_file() {
            if !entry.file_name().to_string_lossy().ends_with(".jsonl") {
                continue;
            }
            if files.len() >= limit {
                return Err(TooMany { limit });
            }
            files.push(entry.path());
        } else if kind.is_dir() {
            folders.push(entry.path());
        }
    }
    for batch in folders.chunks(READERS) {
        let listed =
            futures::future::join_all(batch.iter().map(|folder| jsonl_names(folder))).await;
        for (folder, names) in batch.iter().zip(listed) {
            for name in names {
                if files.len() >= limit {
                    return Err(TooMany { limit });
                }
                files.push(folder.join(name));
            }
        }
    }
    Ok(files)
}

/// The files among `files` whose header carries `durable_session_id`, read `READERS` at a time, with each header's cwd.
async fn carrying(files: &[PathBuf], durable_session_id: &str) -> Vec<(PathBuf, String)> {
    let mut matches = Vec::new();
    for batch in files.chunks(READERS) {
        let headers = futures::future::join_all(batch.iter().map(|file| header_of(file))).await;
        for (file, header) in batch.iter().zip(headers) {
            let Some((id, cwd)) = header else {
                continue;
            };
            if id == durable_session_id {
                matches.push((file.clone(), cwd));
            }
        }
    }
    matches
}

/// senpi `findSessionFile`: the hosted rows first (an engine holds those), then the session files named for the id whose
/// header carries it, or when none does every session file whose header carries it (senpi#7136). One file answers
/// `found`; two answer `ambiguous` and name neither.
async fn find_session_file(
    agent_dir: &Path,
    durable_session_id: &str,
    hosted: &[Value],
    limit: usize,
) -> Result<Value, TooMany> {
    let files = session_files(agent_dir, limit).await?;
    let suffix = format!("_{durable_session_id}.jsonl");
    let named: Vec<PathBuf> = files
        .iter()
        .filter(|file| {
            file.file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(&suffix))
        })
        .cloned()
        .collect();
    let mut matches = carrying(&named, durable_session_id).await;
    if matches.is_empty() {
        matches = carrying(&files, durable_session_id).await;
    }
    let mut by_path: BTreeMap<PathBuf, Candidate> = BTreeMap::new();
    for row in hosted {
        if row.get("durableSessionId").and_then(Value::as_str) != Some(durable_session_id) {
            continue;
        }
        let Some(session_path) = row.get("sessionPath").and_then(Value::as_str) else {
            continue;
        };
        by_path.insert(
            canonical_session_path(Path::new(session_path)),
            Candidate {
                session_path: session_path.to_owned(),
                cwd: row
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                open: true,
            },
        );
    }
    for (file, cwd) in matches {
        // A hosted row for the same file already says an engine holds it; the file never takes that back.
        by_path
            .entry(canonical_session_path(&file))
            .or_insert_with(|| Candidate {
                session_path: file.to_string_lossy().into_owned(),
                cwd,
                open: false,
            });
    }
    let mut found = by_path.into_values();
    Ok(match (found.next(), found.next()) {
        (None, _) => json!({"unknown": true}),
        (Some(only), None) => {
            json!({"found": {"sessionPath": only.session_path, "cwd": only.cwd, "open": only.open}})
        }
        (Some(_), Some(_)) => json!({"ambiguous": true}),
    })
}

fn envelope(command: &Value) -> Map<String, Value> {
    let mut response = Map::new();
    if let Some(id) = command.get("id").filter(|id| !id.is_null()) {
        response.insert("id".into(), id.clone());
    }
    response.insert("type".into(), "response".into());
    response.insert("command".into(), "find_session".into());
    response
}

fn answer(command: &Value, data: Value) -> Value {
    let mut response = envelope(command);
    response.insert("success".into(), true.into());
    response.insert("data".into(), data);
    Value::Object(response)
}

fn refusal(command: &Value, code: &str, detail: &str) -> Value {
    let mut response = envelope(command);
    response.insert("success".into(), false.into());
    response.insert("error".into(), code.into());
    response.insert("errorCode".into(), code.into());
    response.insert("errorData".into(), json!({ "detail": detail }));
    Value::Object(response)
}

/// senpi `findSession(port, command)`: the RPC face. It decides nothing itself beyond the request's shape and this
/// host's own folder. `agent_dir` is the directory whose `sessions/` folder is searched and `hosted` the host's live
/// session rows (the `list_sessions` rows, workers included).
pub async fn find_session(agent_dir: Option<&Path>, hosted: &[Value], command: &Value) -> Value {
    find_session_within(agent_dir, hosted, command, FIND_SESSION_FILE_LIMIT).await
}

async fn find_session_within(
    agent_dir: Option<&Path>,
    hosted: &[Value],
    command: &Value,
    limit: usize,
) -> Value {
    let durable_session_id = match command.get("durableSessionId").and_then(Value::as_str) {
        Some(id) if !id.trim().is_empty() => id,
        _ => {
            return refusal(
                command,
                RPC_ERROR_INVALID_ARGUMENT,
                "find_session needs durableSessionId, the session's own id.",
            );
        }
    };
    let Some(agent_dir) = agent_dir else {
        return refusal(
            command,
            RPC_ERROR_FIND_SESSION_UNSUPPORTED,
            "This host keeps no agent directory, so it holds no session files to look in.",
        );
    };
    match find_session_file(agent_dir, durable_session_id, hosted, limit).await {
        Ok(data) => answer(command, data),
        Err(too_many) => refusal(command, RPC_ERROR_FIND_SESSION_TOO_MANY, &too_many.detail()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Never a session id the fixture host already knows: two files with one id answer ambiguous.
    const ID: &str = "0197f6e4-4cf9-7f44-a2d8-f8f7f49ee9e5";
    const OTHER_ID: &str = "0197f6e4-4cf9-7f44-a2d8-f8f7f49ee9d7";

    fn session_dir(agent_dir: &Path, folder: &str) -> PathBuf {
        let dir = agent_dir.join("sessions").join(folder);
        std::fs::create_dir_all(&dir).expect("session folder");
        dir
    }

    fn write_session_file(dir: &Path, id: &str, stamp: &str) -> PathBuf {
        let file = dir.join(format!("{stamp}_{id}.jsonl"));
        let header = json!({"type":"session","version":3,"id":id,"timestamp":"2026-11-11T00:00:00.000Z","cwd":dir});
        let message = json!({"type":"message","id":"u1","parentId":null,"timestamp":"2026-11-11T00:00:01.000Z","message":{"role":"user","content":"remember 42","timestamp":1}});
        std::fs::write(&file, format!("{header}\n{message}\n")).expect("session file");
        file
    }

    fn request(durable_session_id: Value) -> Value {
        json!({"type":"find_session","id":"fs","durableSessionId":durable_session_id})
    }

    // senpi#6429 "a session file that exists on disk and is not open": the answer is the file's path and its
    // header's cwd, and the file's bytes are unchanged.
    #[tokio::test]
    async fn a_file_on_disk_is_found_with_its_header_cwd_and_stays_untouched() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = session_dir(temp.path(), "--work--");
        let file = write_session_file(&dir, ID, "2026-11-11T00-00-00-000Z");
        let before = std::fs::read(&file).expect("read");

        let answered = find_session(Some(temp.path()), &[], &request(json!(ID))).await;

        assert_eq!(
            answered,
            json!({
                "id": "fs",
                "type": "response",
                "command": "find_session",
                "success": true,
                "data": {"found": {"sessionPath": file, "cwd": dir, "open": false}},
            })
        );
        assert_eq!(std::fs::read(&file).expect("read"), before);
    }

    // senpi#6429 "two session files that carry one durable id": ambiguous, naming neither.
    #[tokio::test]
    async fn two_files_with_one_id_are_ambiguous_and_name_neither() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_session_file(
            &session_dir(temp.path(), "--a--"),
            ID,
            "2026-11-11T00-00-00-000Z",
        );
        write_session_file(
            &session_dir(temp.path(), "--b--"),
            ID,
            "2026-11-11T01-00-00-000Z",
        );

        let answered = find_session(Some(temp.path()), &[], &request(json!(ID))).await;

        assert_eq!(answered["data"], json!({"ambiguous": true}));
        assert!(!answered.to_string().contains(".jsonl"));
    }

    // senpi#7017: a session started with `sessions/` itself as its session dir keeps its file directly in it, so a
    // root file is a candidate like a folder's.
    #[tokio::test]
    async fn a_session_file_directly_under_the_session_root_is_found() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("sessions");
        std::fs::create_dir_all(&root).expect("session root");
        let file = root.join(format!("2026-11-11T00-00-00-000Z_{ID}.jsonl"));
        let header = json!({"type":"session","version":3,"id":ID,"cwd":root});
        std::fs::write(&file, format!("{header}\n")).expect("session file");

        let answered = find_session(Some(temp.path()), &[], &request(json!(ID))).await;

        assert_eq!(
            answered["data"],
            json!({"found": {"sessionPath": file, "cwd": root, "open": false}})
        );
    }

    #[tokio::test]
    async fn an_id_no_file_carries_is_unknown() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_session_file(
            &session_dir(temp.path(), "--a--"),
            ID,
            "2026-11-11T00-00-00-000Z",
        );

        let answered = find_session(Some(temp.path()), &[], &request(json!(OTHER_ID))).await;

        assert_eq!(answered["success"], true);
        assert_eq!(answered["data"], json!({"unknown": true}));
    }

    // senpi#7021 review: the root scan adds only session files that carry the id.
    #[tokio::test]
    async fn root_files_that_are_no_session_or_carry_another_id_answer_unknown() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("sessions");
        std::fs::create_dir_all(&root).expect("session root");
        write_session_file(&root, OTHER_ID, "2026-11-11T00-00-00-000Z");
        std::fs::write(
            root.join(format!("2026-11-11T02-00-00-000Z_{ID}.jsonl")),
            "not a session header\n",
        )
        .expect("file");

        let answered = find_session(Some(temp.path()), &[], &request(json!(ID))).await;

        assert_eq!(answered["data"], json!({"unknown": true}));
    }

    // senpi#7136: the file senpi named for the id answers, and a file under another name whose header carries the same id
    // is not a second candidate.
    #[tokio::test]
    async fn the_named_file_answers_and_a_renamed_copy_beside_it_is_not_read() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = session_dir(temp.path(), "--a--");
        let file = write_session_file(&dir, ID, "2026-11-11T00-00-00-000Z");
        let other = session_dir(temp.path(), "--b--");
        std::fs::rename(
            write_session_file(&other, ID, "2026-11-11T01-00-00-000Z"),
            other.join("copy.jsonl"),
        )
        .expect("rename copy");

        let answered = find_session(Some(temp.path()), &[], &request(json!(ID))).await;

        assert_eq!(
            answered["data"],
            json!({"found": {"sessionPath": file, "cwd": dir, "open": false}})
        );
    }

    // senpi#7136: the header decides, never the name alone.
    #[tokio::test]
    async fn a_file_named_for_the_id_whose_header_names_another_id_does_not_answer() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = session_dir(temp.path(), "--a--");
        std::fs::rename(
            write_session_file(&dir, OTHER_ID, "2026-11-11T00-00-00-000Z"),
            dir.join(format!("2026-11-11T00-00-00-000Z_{ID}.jsonl")),
        )
        .expect("rename decoy");
        let real = dir.join("chosen.jsonl");
        std::fs::rename(
            write_session_file(&dir, ID, "2026-11-11T03-00-00-000Z"),
            &real,
        )
        .expect("rename real");

        let answered = find_session(Some(temp.path()), &[], &request(json!(ID))).await;

        assert_eq!(
            answered["data"],
            json!({"found": {"sessionPath": real, "cwd": dir, "open": false}})
        );
    }

    // senpi#7136: a session file under a caller-chosen name is found by the full header pass.
    #[tokio::test]
    async fn a_session_file_under_a_caller_chosen_name_is_found_by_the_full_pass() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = session_dir(temp.path(), "--work--");
        let file = dir.join("chosen.jsonl");
        std::fs::rename(
            write_session_file(&dir, ID, "2026-11-11T00-00-00-000Z"),
            &file,
        )
        .expect("rename");

        let answered = find_session(Some(temp.path()), &[], &request(json!(ID))).await;

        assert_eq!(
            answered["data"],
            json!({"found": {"sessionPath": file, "cwd": dir, "open": false}})
        );
    }

    // senpi#7136: the session folders are listed READERS at a time; a match in a later batch is still found.
    #[tokio::test]
    async fn a_session_in_a_late_folder_batch_is_found() {
        let temp = tempfile::tempdir().expect("tempdir");
        for index in 0..=(READERS * 2) {
            write_session_file(
                &session_dir(temp.path(), &format!("--f{index:03}--")),
                OTHER_ID,
                &format!("2026-11-11T00-00-{index:02}-000Z"),
            );
        }
        let dir = session_dir(temp.path(), "--zz-last--");
        let file = write_session_file(&dir, ID, "2026-11-11T09-00-00-000Z");

        let answered = find_session(Some(temp.path()), &[], &request(json!(ID))).await;

        assert_eq!(
            answered["data"],
            json!({"found": {"sessionPath": file, "cwd": dir, "open": false}})
        );
    }

    // senpi#6429 "a session this host has open": its row names the file and says an engine holds it, and the file
    // on disk never takes that back or answers a second candidate.
    #[tokio::test]
    async fn a_hosted_row_answers_open_and_wins_over_its_own_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = session_dir(temp.path(), "--work--");
        let file = write_session_file(&dir, ID, "2026-11-11T00-00-00-000Z");
        let hosted = [
            json!({"sessionId":"s1","durableSessionId":ID,"sessionPath":file,"cwd":"/hosted/cwd"}),
            json!({"sessionId":"s2","durableSessionId":OTHER_ID,"sessionPath":"/elsewhere.jsonl","cwd":"/x"}),
            json!({"sessionId":"s3","durableSessionId":ID,"cwd":"/no/file"}),
        ];

        let answered = find_session(Some(temp.path()), &hosted, &request(json!(ID))).await;

        assert_eq!(
            answered["data"],
            json!({"found": {"sessionPath": file, "cwd": "/hosted/cwd", "open": true}})
        );
    }

    // senpi#6429 "a request with no durable id, an empty one or one that is not a string".
    #[tokio::test]
    async fn a_missing_empty_or_non_string_id_is_refused_before_anything_is_scanned() {
        let temp = tempfile::tempdir().expect("tempdir");
        for durable_session_id in [json!(null), json!(""), json!("   "), json!(7)] {
            let answered = find_session(
                Some(temp.path()),
                &[],
                &json!({"type":"find_session","id":"fs","durableSessionId":durable_session_id}),
            )
            .await;
            assert_eq!(answered["success"], false);
            assert_eq!(answered["errorCode"], "invalid_argument");
        }
        let absent = find_session(
            Some(temp.path()),
            &[],
            &json!({"type":"find_session","id":"fs"}),
        )
        .await;
        assert_eq!(absent["errorCode"], "invalid_argument");
        assert!(!temp.path().join("sessions").exists());
    }

    // senpi#6429 "more session files than one find_session reads": a typed error, never a partial answer, and no
    // file is read before the bound is decided (the lookup is run with a small bound instead of 20,001 files).
    #[tokio::test]
    async fn more_files_than_the_bound_answer_a_typed_error_instead_of_a_partial_answer() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = session_dir(temp.path(), "--crowd--");
        write_session_file(&dir, ID, "2026-11-11T00-00-00-000Z");
        for index in 0..3 {
            std::fs::write(dir.join(format!("f{index}.jsonl")), "").expect("filler");
        }

        let refused = find_session_within(Some(temp.path()), &[], &request(json!(ID)), 3).await;

        assert_eq!(refused["success"], false);
        assert_eq!(refused["errorCode"], RPC_ERROR_FIND_SESSION_TOO_MANY);
        assert_eq!(refused["error"], RPC_ERROR_FIND_SESSION_TOO_MANY);
        assert!(
            refused["errorData"]["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("more than 3 session files"))
        );
        // The same files within a larger bound answer normally.
        let answered = find_session_within(Some(temp.path()), &[], &request(json!(ID)), 4).await;
        assert_eq!(answered["success"], true);
        assert_eq!(answered["data"]["found"]["open"], false);
    }

    // senpi#6429 "an agent directory with no session folder at all": unknown, and nothing is created.
    #[tokio::test]
    async fn an_agent_directory_without_a_session_folder_answers_unknown_and_creates_nothing() {
        let temp = tempfile::tempdir().expect("tempdir");

        let answered = find_session(Some(temp.path()), &[], &request(json!(ID))).await;

        assert_eq!(answered["data"], json!({"unknown": true}));
        assert!(!temp.path().join("sessions").exists());
    }

    #[tokio::test]
    async fn a_host_without_an_agent_directory_answers_unsupported() {
        let answered = find_session(None, &[], &request(json!(ID))).await;

        assert_eq!(answered["success"], false);
        assert_eq!(answered["errorCode"], RPC_ERROR_FIND_SESSION_UNSUPPORTED);
    }

    // A first line that is not a session header, or no newline within the header budget, is no candidate.
    #[tokio::test]
    async fn only_a_complete_session_header_makes_a_file_a_candidate() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = session_dir(temp.path(), "--work--");
        let not_a_header = json!({"type":"message","id":ID});
        std::fs::write(dir.join("message.jsonl"), format!("{not_a_header}\n")).expect("file");
        std::fs::write(dir.join("garbage.jsonl"), "not json\n").expect("file");
        std::fs::write(dir.join("notes.txt"), "ignored").expect("file");
        let padded =
            json!({"type":"session","id":ID,"cwd":"/x","pad":"x".repeat(HEADER_READ_BYTES)});
        std::fs::write(dir.join("padded.jsonl"), padded.to_string()).expect("file");

        let answered = find_session(Some(temp.path()), &[], &request(json!(ID))).await;

        assert_eq!(answered["data"], json!({"unknown": true}));
    }

    // A session header without a cwd names an empty folder, as senpi answers it.
    #[tokio::test]
    async fn a_header_without_a_cwd_answers_an_empty_folder() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = session_dir(temp.path(), "--work--");
        let file = dir.join("s.jsonl");
        std::fs::write(&file, format!("{}\n", json!({"type":"session","id":ID}))).expect("file");

        let answered = find_session(Some(temp.path()), &[], &request(json!(ID))).await;

        assert_eq!(
            answered["data"],
            json!({"found": {"sessionPath": file, "cwd": "", "open": false}})
        );
    }
}
