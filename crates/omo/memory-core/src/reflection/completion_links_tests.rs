use super::*;
use crate::git::exec::system_git_exec;
use crate::git::{GitMemoryRepo, GitMemoryRepoOptions, GitSeedFile, InitializeGitRepoOptions};
use crate::reflection::completion_validation::{
    CompletionValidation, paths_outside_target, validate_completion,
};
use crate::reflection::worktree::create_reflection_worktree;

const INDEX: &str =
    "---\ndescription: Index of plans\n---\n\nSee [[reference/plan]] and [plan](plan.md).\n";
const PLAN: &str = "---\ndescription: The plan\n---\n\nplan body\n";

fn git(exec: &dyn GitExec, dir: &Path, args: &[&str]) -> String {
    let result = exec.run_in(dir, args).expect("git run");
    assert_eq!(result.code, 0, "git {}: {}", args.join(" "), result.stderr);
    result.stdout
}

#[test]
fn a_run_that_deletes_a_note_repoints_the_links_to_it_in_the_same_run() {
    let temp = tempfile::TempDir::new().unwrap();
    let repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: temp.path().join("memory"),
        agent_id: "agent-one".to_string(),
        exec: None,
        install_hooks: None,
    })
    .unwrap();
    repo.init(InitializeGitRepoOptions {
        seed_files: vec![
            GitSeedFile {
                relative_path: "reference/index.md".to_string(),
                content: INDEX.to_string(),
            },
            GitSeedFile {
                relative_path: "reference/plan.md".to_string(),
                content: PLAN.to_string(),
            },
        ],
        author_name: Some("Reflection Agent".to_string()),
        install_hooks: None,
    })
    .unwrap();
    let exec = system_git_exec();
    let worktree = create_reflection_worktree(
        &repo,
        "run-links",
        &temp.path().join("worktrees"),
        exec.as_ref(),
        None,
    )
    .unwrap();
    let base = worktree.base_sha().to_owned();
    git(
        exec.as_ref(),
        &worktree.dir,
        &["rm", "-q", "reference/plan.md"],
    );
    git(
        exec.as_ref(),
        &worktree.dir,
        &[
            "-c",
            "user.email=fixture@example.com",
            "-c",
            "user.name=fixture",
            "commit",
            "-qm",
            "forget the plan",
        ],
    );

    let first = validate_completion(&worktree, &base, exec.as_ref());
    let second = validate_completion(&worktree, &base, exec.as_ref());

    let CompletionValidation::Valid {
        tip_sha,
        changed_paths,
        link_repairs,
        ..
    } = &first
    else {
        panic!("expected a valid completion, got {first:?}");
    };
    assert_eq!(link_repairs, &vec!["reference/index.md".to_owned()]);
    assert!(changed_paths.contains(&"reference/index.md".to_owned()));
    assert!(changed_paths.contains(&"reference/plan.md".to_owned()));
    let index = git(
        exec.as_ref(),
        &worktree.dir,
        &["show", "HEAD:reference/index.md"],
    );
    assert!(index.contains("See reference/plan and plan.\n"), "{index}");
    match &second {
        CompletionValidation::Valid {
            tip_sha: again,
            link_repairs: repairs_again,
            ..
        } => {
            assert_eq!(again, tip_sha);
            assert_eq!(repairs_again, link_repairs);
        }
        other => panic!("expected the same valid completion, got {other:?}"),
    }
    assert_eq!(
        paths_outside_target(
            changed_paths,
            link_repairs,
            &["reference/plan.md".to_owned()]
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        paths_outside_target(changed_paths, &[], &["reference/plan.md".to_owned()]),
        vec!["reference/index.md".to_owned()]
    );
}
