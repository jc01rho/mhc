use super::{
    constants::*,
    scanner::{ScannedFile, scan_rule_files},
    types::RuleCandidate,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

const HEPHAESTUS_BUNDLED_RULE_PREFIX: &str = "bundled-rules/hephaestus/";
const HEPHAESTUS_DEFAULT_VARIANT_FILE: &str = "gpt-5.5.md";
const HEPHAESTUS_MODEL_VARIANT_FILES: &[(&str, &str)] =
    &[("gpt-5.6", "gpt-5.6.md"), ("gpt-6", "gpt-6.md")];

pub fn hephaestus_variant_file_for_model(model: Option<&str>) -> &'static str {
    let model = model.unwrap_or_default().to_ascii_lowercase();
    HEPHAESTUS_MODEL_VARIANT_FILES
        .iter()
        .find(|(family, _)| model.contains(family))
        .map_or(HEPHAESTUS_DEFAULT_VARIANT_FILE, |(_, file)| file)
}

fn is_model_id_boundary(byte: u8) -> bool {
    matches!(byte, b'/' | b'@' | b':' | b'.' | b'_' | b'-')
}

fn is_model_id_separator(byte: u8) -> bool {
    matches!(byte, b'.' | b'_' | b'-')
}

/// One match of senpi's `hasGpt62Signal` (`(?:^|[/@:._-])gpt[._-]?6(?:[._-]2|2)[._-](?:astra|sol)
/// (?:$|[/@:._-])`) starting at `start`, the index right after "gpt".
fn gpt_6_2_signal_at(id: &[u8], start: usize) -> bool {
    let mut at = start;
    if id.get(at).copied().is_some_and(is_model_id_separator) {
        at += 1;
    }
    if id.get(at) != Some(&b'6') {
        return false;
    }
    at += 1;
    if id.get(at).copied().is_some_and(is_model_id_separator) {
        at += 1;
    }
    if id.get(at) != Some(&b'2') {
        return false;
    }
    at += 1;
    if !id.get(at).copied().is_some_and(is_model_id_separator) {
        return false;
    }
    at += 1;
    let Some(rest) = id.get(at..) else {
        return false;
    };
    [b"astra".as_slice(), b"sol".as_slice()].iter().any(|tier| {
        rest.starts_with(tier)
            && rest
                .get(tier.len())
                .is_none_or(|byte| is_model_id_boundary(*byte))
    })
}

/// Whether `model` is a GPT-6.2 Astra or Sol id or display name: the models senpi's `gpt-6.2` prompt
/// preset serves (`hasGpt62Signal`, senpi 6d4426a28), provider prefixes and Venice's dotless
/// `openai-gpt-62-astra` included.
pub fn is_gpt_6_2_model(model: &str) -> bool {
    let normalized = model
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");
    let id = normalized.as_bytes();
    id.windows(3).enumerate().any(|(index, window)| {
        window == b"gpt"
            && (index == 0 || id.get(index - 1).copied().is_some_and(is_model_id_boundary))
            && gpt_6_2_signal_at(id, index + 3)
    })
}

/// A bundled Hephaestus variant is enabled only for the active model's variant, and never on a
/// GPT-6.2 model: that preset stands alone and takes no Hephaestus role prompt (omo #14186).
pub fn is_hephaestus_candidate_enabled(relative_path: &str, model: Option<&str>) -> bool {
    if !relative_path.starts_with(HEPHAESTUS_BUNDLED_RULE_PREFIX) {
        return true;
    }
    if model.is_some_and(is_gpt_6_2_model) {
        return false;
    }
    relative_path
        == format!(
            "{HEPHAESTUS_BUNDLED_RULE_PREFIX}{}",
            hephaestus_variant_file_for_model(model)
        )
}

#[derive(Default)]
pub struct RuleDiscoveryCache {
    pub scanned_rule_files: BTreeMap<PathBuf, Vec<ScannedFile>>,
    pub single_file_info: BTreeMap<PathBuf, Option<PathBuf>>,
}

pub struct FinderOptions<'a> {
    pub project_root: Option<&'a Path>,
    pub target_file: Option<&'a Path>,
    pub home_dir: &'a Path,
    pub disabled_sources: &'a BTreeSet<String>,
    pub skip_user_home: bool,
}

pub fn find_rule_candidates(
    options: FinderOptions<'_>,
    cache: &mut RuleDiscoveryCache,
) -> Vec<RuleCandidate> {
    let mut candidates = Vec::new();
    if let Some(root) = options.project_root {
        let root = absolute(root);
        let start = options
            .target_file
            .map(absolute)
            .and_then(|path| path.parent().map(Path::to_path_buf));
        let mut directories = Vec::new();
        let mut current = start
            .filter(|path| {
                path.strip_prefix(&root)
                    .is_ok_and(|relative| !relative.to_string_lossy().starts_with(".."))
            })
            .unwrap_or_else(|| root.clone());
        loop {
            directories.push(current.clone());
            if current == root {
                break;
            }
            let Some(parent) = current.parent() else {
                break;
            };
            current = parent.to_path_buf();
        }
        for (distance, directory) in directories.iter().enumerate() {
            for (parent, subdir) in PROJECT_RULE_SUBDIRS {
                let source = format!("{parent}/{subdir}");
                if !options.disabled_sources.contains(&source) {
                    add_directory(
                        &mut candidates,
                        cache,
                        &directory.join(parent).join(subdir),
                        &root,
                        &source,
                        distance,
                        false,
                    );
                }
            }
        }
        for (distance, directory) in directories.iter().enumerate() {
            for source in PROJECT_SINGLE_FILES {
                if !options.disabled_sources.contains(*source) {
                    add_file(
                        &mut candidates,
                        cache,
                        &directory.join(source),
                        &root,
                        source,
                        distance,
                        false,
                    );
                }
            }
        }
    }
    if !options.skip_user_home {
        let home = absolute(options.home_dir);
        for subdir in USER_HOME_RULE_SUBDIRS {
            let source = format!("~/{subdir}");
            if !options.disabled_sources.contains(&source) {
                add_directory(
                    &mut candidates,
                    cache,
                    &home.join(subdir),
                    &home,
                    &source,
                    GLOBAL_DISTANCE,
                    true,
                );
            }
        }
        for file in USER_HOME_SINGLE_FILES {
            let source = format!("~/{file}");
            if !options.disabled_sources.contains(&source) {
                add_file(
                    &mut candidates,
                    cache,
                    &home.join(file),
                    &home,
                    &source,
                    GLOBAL_DISTANCE,
                    true,
                );
            }
        }
    }
    candidates
}

pub(crate) fn absolute(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            std::path::Component::Normal(_)
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => normalized.push(component.as_os_str()),
        }
    }
    normalized
}
fn candidate(
    path: &Path,
    real_path: PathBuf,
    root: &Path,
    source: &str,
    distance: usize,
    global: bool,
    single: bool,
) -> RuleCandidate {
    RuleCandidate {
        path: path.to_string_lossy().into_owned(),
        real_path: real_path.to_string_lossy().into_owned(),
        source: source.into(),
        distance,
        is_global: global,
        is_single_file: single,
        relative_path: path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/"),
    }
}
fn add_directory(
    output: &mut Vec<RuleCandidate>,
    cache: &mut RuleDiscoveryCache,
    directory: &Path,
    root: &Path,
    source: &str,
    distance: usize,
    global: bool,
) {
    let files = cache
        .scanned_rule_files
        .entry(directory.to_path_buf())
        .or_insert_with(|| scan_rule_files(directory, None, None));
    for file in files {
        output.push(candidate(
            &file.path,
            std::fs::canonicalize(&file.path).unwrap_or_else(|_| file.path.clone()),
            root,
            source,
            distance,
            global,
            false,
        ));
    }
}
fn add_file(
    output: &mut Vec<RuleCandidate>,
    cache: &mut RuleDiscoveryCache,
    path: &Path,
    root: &Path,
    source: &str,
    distance: usize,
    global: bool,
) {
    let real = cache
        .single_file_info
        .entry(path.to_path_buf())
        .or_insert_with(|| {
            path.is_file()
                .then(|| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
        });
    if let Some(real) = real {
        output.push(candidate(
            path,
            real.clone(),
            root,
            source,
            distance,
            global,
            true,
        ));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn discovers_pi_rule_sources_with_upstream_priority() {
        let root = std::env::temp_dir().join(format!("maho-rules-pi-{}", std::process::id()));
        let home = root.join("home");
        let project = root.join("project");
        fs::create_dir_all(project.join(".pi/rules")).expect("create project rules");
        fs::create_dir_all(home.join(".pi/rules")).expect("create home rules");
        fs::write(project.join(".pi/rules/project.md"), "project").expect("write project rule");
        fs::write(home.join(".pi/rules/home.md"), "home").expect("write home rule");
        let disabled = BTreeSet::new();
        let mut cache = RuleDiscoveryCache::default();
        let candidates = find_rule_candidates(
            FinderOptions {
                project_root: Some(&project),
                target_file: None,
                home_dir: &home,
                disabled_sources: &disabled,
                skip_user_home: false,
            },
            &mut cache,
        );
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.source == ".pi/rules"
                    && candidate.path.ends_with("project.md"))
        );
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.source == "~/.pi/rules"
                    && candidate.path.ends_with("home.md"))
        );
        assert_eq!(
            SOURCE_PRIORITY
                .iter()
                .find(|(source, _)| *source == ".pi/rules")
                .map(|(_, priority)| *priority),
            Some(0)
        );
        let _ = fs::remove_dir_all(root);
    }
}
