//! Translation of `server-resolution-typescript-native.test.ts`.

use crate::support::typescript_projects::TypescriptProjects;
use crate::support::typescript_projects::context_for;
use crate::support::typescript_projects::install_typescript;
use crate::support::typescript_projects::path_string;
use crate::support::typescript_projects::user_config_path;
use crate::support::typescript_projects::write_executable;
use lsp_core::lsp::server_installation::ResolveServerBinaryOptions;
use lsp_core::lsp::server_installation::reset_server_binary_resolution_cache_for_tests;
use lsp_core::lsp::server_resolution::find_server_for_extension_with_options;
use lsp_core::lsp::server_resolution::get_all_servers_with_options;
use lsp_core::lsp::types::ResolvedServer;
use lsp_core::lsp::types::ServerLookupResult;
use lsp_core::request_context::run_with_request_context;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::path::PathBuf;

fn projects() -> TypescriptProjects {
    reset_server_binary_resolution_cache_for_tests();
    TypescriptProjects::new("lsp-ts-native-")
}

fn binary_options(path_env: &Path) -> ResolveServerBinaryOptions {
    ResolveServerBinaryOptions {
        path_env: Some(path_string(path_env)),
        ..ResolveServerBinaryOptions::default()
    }
}

fn empty_path(root: &Path) -> PathBuf {
    root.join("definitely-empty")
}

fn lookup_with_path(session: &Path, target: Option<&Path>, path_env: &Path) -> ServerLookupResult {
    let options = binary_options(path_env);
    let target = target.map(path_string);
    run_with_request_context(context_for(session), || {
        find_server_for_extension_with_options(".ts", target.as_deref(), &options)
    })
    .expect("request context")
}

fn lookup(session: &Path, target: Option<&Path>) -> ServerLookupResult {
    lookup_with_path(session, target, &empty_path(session))
}

fn found(result: ServerLookupResult) -> ResolvedServer {
    match result {
        ServerLookupResult::Found { server } => server,
        other => panic!("expected a found server, got {other:?}"),
    }
}

fn native(tsc: &str) -> Vec<String> {
    vec![tsc.to_string(), "--lsp".to_string(), "--stdio".to_string()]
}

fn language_server_command(language_server: &str) -> Vec<String> {
    vec![language_server.to_string(), "--stdio".to_string()]
}

fn write_user_config(root: &Path, config: &Value) {
    let path = user_config_path(root);
    std::fs::create_dir_all(path.parent().expect("config parent")).expect("create home-config");
    std::fs::write(path, config.to_string()).expect("write user config");
}

#[test]
fn given_a_project_on_typescript_7_when_looking_up_a_typescript_file_then_its_own_tsc_serves_lsp_without_tsserver_options()
 {
    // given: typescript-language-server is installed too, but it cannot start without lib/tsserver.js
    let mut projects = projects();
    let root = projects.make_project();
    let tsc = install_typescript(&root.join("node_modules"), 7);
    write_executable(
        &root
            .join("node_modules")
            .join(".bin")
            .join("typescript-language-server"),
    );

    // when
    let server = found(lookup(&root, None));

    // then
    assert_eq!(server.id, "typescript");
    assert_eq!(server.command, native(&tsc));
    assert_eq!(server.initialization, None);
}

#[test]
fn given_a_project_on_typescript_5_when_looking_up_a_typescript_file_then_typescript_language_server_still_serves_it()
 {
    // given
    let mut projects = projects();
    let root = projects.make_project();
    install_typescript(&root.join("node_modules"), 5);
    let language_server = write_executable(
        &root
            .join("node_modules")
            .join(".bin")
            .join("typescript-language-server"),
    );

    // when
    let server = found(lookup(&root, None));

    // then
    assert_eq!(server.command, language_server_command(&language_server));
    assert!(server.initialization.is_some());
}

#[test]
fn given_typescript_7_only_on_path_when_looking_up_then_the_project_less_compiler_does_not_replace_the_builtin_server()
 {
    // given
    let mut projects = projects();
    let root = projects.make_project();
    let global_prefix = projects.make_project();
    install_typescript(&global_prefix.join("node_modules"), 7);

    // when
    let result = lookup_with_path(
        &root,
        None,
        &global_prefix.join("node_modules").join(".bin"),
    );

    // then
    match result {
        ServerLookupResult::NotInstalled { server, .. } => assert_eq!(server.id, "typescript"),
        other => panic!("expected not_installed, got {other:?}"),
    }
}

#[test]
fn given_a_user_typescript_server_with_its_own_command_when_the_project_is_on_typescript_7_then_the_configured_command_wins()
 {
    // given
    let mut projects = projects();
    let root = projects.make_project();
    install_typescript(&root.join("node_modules"), 7);
    let custom = write_executable(&root.join("node_modules").join(".bin").join("custom-ts-ls"));
    write_user_config(
        &root,
        &json!({ "lsp": { "typescript": { "command": ["custom-ts-ls", "--stdio"], "extensions": [".ts"] } } }),
    );

    // when
    let server = found(lookup(&root, None));

    // then
    assert_eq!(server.command, language_server_command(&custom));
}

#[test]
fn given_a_project_on_typescript_7_alone_when_listing_servers_then_typescript_reports_installed() {
    // given
    let mut projects = projects();
    let root = projects.make_project();
    install_typescript(&root.join("node_modules"), 7);
    let options = binary_options(&empty_path(&root));

    // when
    let servers = run_with_request_context(context_for(&root), || {
        get_all_servers_with_options(&options)
    })
    .expect("request context");

    // then
    let typescript = servers
        .iter()
        .find(|server| server.id == "typescript")
        .expect("typescript server status");
    assert!(typescript.installed);
}

#[test]
fn given_a_monorepo_session_root_without_typescript_7_when_looking_up_a_file_per_package_then_each_packages_own_typescript_decides()
 {
    // given: the root has typescript-language-server only; packages/x is on TypeScript 7, packages/y on TypeScript 5
    let mut projects = projects();
    let root = projects.make_project();
    std::fs::create_dir(root.join(".git")).expect("create .git");
    let language_server = write_executable(
        &root
            .join("node_modules")
            .join(".bin")
            .join("typescript-language-server"),
    );
    let package_x = root.join("packages").join("x");
    let package_y = root.join("packages").join("y");
    std::fs::create_dir_all(package_x.join("src")).expect("create packages/x/src");
    std::fs::create_dir_all(package_y.join("src")).expect("create packages/y/src");
    std::fs::write(package_x.join("package.json"), "{}\n").expect("write x package.json");
    std::fs::write(package_y.join("package.json"), "{}\n").expect("write y package.json");
    let tsc_x = install_typescript(&package_x.join("node_modules"), 7);
    install_typescript(&package_y.join("node_modules"), 5);
    let options = binary_options(&empty_path(&root));
    let target_x = path_string(&package_x.join("src").join("broken.ts"));
    let target_y = path_string(&package_y.join("src").join("fine.ts"));

    // when
    let (in_x, in_y) = run_with_request_context(context_for(&root), || {
        (
            find_server_for_extension_with_options(".ts", Some(&target_x), &options),
            find_server_for_extension_with_options(".ts", Some(&target_y), &options),
        )
    });

    // then
    assert_eq!(found(in_x.expect("x lookup")).command, native(&tsc_x));
    assert_eq!(
        found(in_y.expect("y lookup")).command,
        language_server_command(&language_server)
    );
}

#[test]
fn given_a_user_typescript_command_equal_to_the_default_when_the_project_is_on_typescript_7_then_the_command_and_its_initialization_are_kept()
 {
    // given
    let mut projects = projects();
    let root = projects.make_project();
    install_typescript(&root.join("node_modules"), 7);
    let language_server = write_executable(
        &root
            .join("node_modules")
            .join(".bin")
            .join("typescript-language-server"),
    );
    let initialization = json!({ "preferences": { "quotePreference": "double" } });
    write_user_config(
        &root,
        &json!({ "lsp": { "typescript": { "command": ["typescript-language-server", "--stdio"], "initialization": initialization } } }),
    );

    // when
    let server = found(lookup(&root, None));

    // then
    assert_eq!(server.command, language_server_command(&language_server));
    assert_eq!(
        server.initialization.map(Value::Object),
        Some(initialization)
    );
}

#[test]
fn given_user_initialization_on_the_inherited_typescript_command_when_the_project_is_on_typescript_7_then_tsc_receives_only_that_initialization()
 {
    // given
    let mut projects = projects();
    let root = projects.make_project();
    let tsc = install_typescript(&root.join("node_modules"), 7);
    let initialization = json!({ "locale": "en" });
    write_user_config(
        &root,
        &json!({ "lsp": { "typescript": { "initialization": initialization } } }),
    );

    // when
    let server = found(lookup(&root, None));

    // then
    assert_eq!(server.command, native(&tsc));
    assert_eq!(
        server.initialization.map(Value::Object),
        Some(initialization)
    );
}

struct SessionWithLanguageServer {
    session: PathBuf,
    language_server: String,
}

fn session_with_language_server(projects: &mut TypescriptProjects) -> SessionWithLanguageServer {
    let session = projects.make_project();
    std::fs::create_dir(session.join(".git")).expect("create .git");
    let language_server = write_executable(
        &session
            .join("node_modules")
            .join(".bin")
            .join("typescript-language-server"),
    );
    SessionWithLanguageServer {
        session,
        language_server,
    }
}

#[test]
fn given_an_in_session_directory_linked_to_an_external_typescript_7_project_when_looking_up_a_file_through_the_link_then_the_session_keeps_typescript_language_server()
 {
    // given
    let mut projects = projects();
    let SessionWithLanguageServer {
        session,
        language_server,
    } = session_with_language_server(&mut projects);
    let external = projects.make_project();
    install_typescript(&external.join("node_modules"), 7);
    std::fs::create_dir(external.join("src")).expect("create external src");
    symlink(&external, session.join("linked")).expect("link external project");

    // when
    let server = found(lookup(
        &session,
        Some(&session.join("linked").join("src").join("a.ts")),
    ));

    // then
    assert_eq!(server.command, language_server_command(&language_server));
}

#[test]
fn given_a_session_reached_through_a_symlinked_root_when_looking_up_a_file_of_its_typescript_7_package_by_the_linked_path_then_that_packages_tsc_serves_it()
 {
    // given
    let mut projects = projects();
    let SessionWithLanguageServer { session, .. } = session_with_language_server(&mut projects);
    let package_x = session.join("packages").join("x");
    std::fs::create_dir_all(package_x.join("src")).expect("create packages/x/src");
    std::fs::write(package_x.join("package.json"), "{}\n").expect("write x package.json");
    let tsc_x = install_typescript(&package_x.join("node_modules"), 7);
    let alias = projects.make_project().join("session-alias");
    symlink(&session, &alias).expect("link session alias");

    // when
    let server = found(lookup(
        &session,
        Some(&alias.join("packages").join("x").join("src").join("a.ts")),
    ));

    // then
    assert_eq!(server.command, native(&tsc_x));
}

#[test]
fn given_a_pnpm_layout_whose_typescript_entry_links_into_an_external_store_when_looking_up_then_the_projects_tsc_shim_serves_lsp()
 {
    // given: node_modules and .bin are real directories; only the typescript entry is a link
    let mut projects = projects();
    let SessionWithLanguageServer { session, .. } = session_with_language_server(&mut projects);
    let store = projects.make_project();
    let stored_typescript = store.join("typescript@7.0.0").join("node_modules");
    install_typescript(&stored_typescript, 7);
    std::fs::create_dir_all(session.join("node_modules")).expect("create node_modules");
    symlink(
        stored_typescript.join("typescript"),
        session.join("node_modules").join("typescript"),
    )
    .expect("link stored typescript");
    let tsc = write_executable(&session.join("node_modules").join(".bin").join("tsc"));

    // when
    let server = found(lookup(&session, Some(&session.join("a.ts"))));

    // then
    assert_eq!(server.command, native(&tsc));
}

#[test]
fn given_a_nearer_bin_tsc_in_a_go_package_and_a_hoisted_typescript_7_when_looking_up_a_file_of_that_package_then_the_hoisted_compiler_serves_it()
 {
    // given: packages/svc is marked only by go.mod and carries an unrelated bin/tsc
    let mut projects = projects();
    let SessionWithLanguageServer { session, .. } = session_with_language_server(&mut projects);
    let hoisted = install_typescript(&session.join("node_modules"), 7);
    let go_package = session.join("packages").join("svc");
    std::fs::create_dir_all(&go_package).expect("create packages/svc");
    std::fs::write(go_package.join("go.mod"), "module svc\n").expect("write go.mod");
    let unrelated = write_executable(&go_package.join("bin").join("tsc"));

    // when
    let server = found(lookup(&session, Some(&go_package.join("a.ts"))));

    // then
    assert_eq!(server.command, native(&hoisted));
    assert!(!server.command.contains(&unrelated));
}

#[test]
fn given_a_nearer_node_modules_bin_tsc_without_a_typescript_package_beside_it_and_a_hoisted_typescript_7_when_looking_up_a_file_of_that_package_then_the_hoisted_compiler_serves_it()
 {
    // given: packages/x keeps a stale tsc shim whose typescript package is gone
    let mut projects = projects();
    let SessionWithLanguageServer { session, .. } = session_with_language_server(&mut projects);
    let hoisted = install_typescript(&session.join("node_modules"), 7);
    let package_x = session.join("packages").join("x");
    std::fs::create_dir_all(&package_x).expect("create packages/x");
    std::fs::write(package_x.join("package.json"), "{}\n").expect("write x package.json");
    let stale = write_executable(&package_x.join("node_modules").join(".bin").join("tsc"));

    // when
    let server = found(lookup(&session, Some(&package_x.join("a.ts"))));

    // then
    assert_eq!(server.command, native(&hoisted));
    assert!(!server.command.contains(&stale));
}

#[test]
fn given_a_nearer_package_on_typescript_5_below_a_hoisted_typescript_7_when_looking_up_a_file_of_that_package_then_typescript_language_server_keeps_serving_it()
 {
    // given
    let mut projects = projects();
    let SessionWithLanguageServer {
        session,
        language_server,
    } = session_with_language_server(&mut projects);
    install_typescript(&session.join("node_modules"), 7);
    let package_y = session.join("packages").join("y");
    std::fs::create_dir_all(&package_y).expect("create packages/y");
    std::fs::write(package_y.join("package.json"), "{}\n").expect("write y package.json");
    install_typescript(&package_y.join("node_modules"), 5);

    // when
    let server = found(lookup(&session, Some(&package_y.join("a.ts"))));

    // then
    assert_eq!(server.command, language_server_command(&language_server));
}

#[test]
fn given_node_modules_bin_itself_linked_to_an_external_typescript_7_project_when_looking_up_then_the_external_tsc_is_not_used()
 {
    // given
    let mut projects = projects();
    let SessionWithLanguageServer {
        session,
        language_server,
    } = session_with_language_server(&mut projects);
    let package_x = session.join("packages").join("x");
    std::fs::create_dir_all(package_x.join("node_modules")).expect("create x node_modules");
    std::fs::write(package_x.join("package.json"), "{}\n").expect("write x package.json");
    let external = projects.make_project();
    install_typescript(&external.join("node_modules"), 7);
    symlink(
        external.join("node_modules").join("typescript"),
        package_x.join("node_modules").join("typescript"),
    )
    .expect("link external typescript");
    symlink(
        external.join("node_modules").join(".bin"),
        package_x.join("node_modules").join(".bin"),
    )
    .expect("link external .bin");

    // when
    let server = found(lookup(&session, Some(&package_x.join("a.ts"))));

    // then
    assert_eq!(server.command, language_server_command(&language_server));
}
