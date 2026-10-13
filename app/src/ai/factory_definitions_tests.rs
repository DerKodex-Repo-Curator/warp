use std::sync::Arc;

use command::blocking::Command as BlockingCommand;
use git2::{Repository, Signature};

use super::*;

struct GitFixture {
    working: PathBuf,
    first: String,
    second: String,
}
#[tokio::test]
async fn factory_branch_child_requires_selected_execution_config() {
    let root = tempfile::tempdir().unwrap();
    let mut client = crate::server::server_api::ai::MockAIClient::new();
    client
        .expect_get_factory_definition_config()
        .withf(|task_id, execution_id| task_id == "child" && execution_id == "42")
        .times(1)
        .returning(|_, _| {
            Ok(
                warp_graphql::queries::execution_config::FactoryExecutionDefinitions {
                    execution_id: cynic::Id::new("42"),
                    parent_run_id: Some(cynic::Id::new("parent")),
                    factory_branch: None,
                    factory_skill_dirs: Vec::new(),
                },
            )
        });
    assert!(
        prepare_child("child", root.path(), Some("42"), &client)
            .await
            .is_err()
    );
    assert!(!root.path().join(".warp").exists());
}
#[tokio::test]
async fn factory_branch_execution_config_missing_commit_fails_without_fallback() {
    let Some(fixture) =
        fixture_process("factory_branch_execution_config_missing_commit_fails_without_fallback")
    else {
        return;
    };
    let config = warp_graphql::queries::execution_config::FactoryBranchExecutionConfig {
        factory_id: cynic::Id::new("factory"),
        branch_id: cynic::Id::new("preview-one"),
        branch: "preview".into(),
        source_sha: "0".repeat(40),
        repository_url: "https://fixture.invalid/definitions".into(),
    };
    assert!(
        prepare_from_config(&fixture.working, "42", Some(&config), &["skills".into()])
            .await
            .is_err()
    );
    assert_eq!(
        Repository::open(&fixture.working)
            .unwrap()
            .head()
            .unwrap()
            .target()
            .unwrap()
            .to_string(),
        fixture.second,
    );
}

fn commit_files(repo: &Repository, files: &[(&str, &str)]) -> String {
    for (path, contents) in files {
        let path = repo.workdir().unwrap().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = Signature::now("Fixture", "fixture@example.invalid").unwrap();
    let parent = repo.head().ok().map(|head| head.peel_to_commit().unwrap());
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "Definition files",
        &tree,
        &parent.iter().collect::<Vec<_>>(),
    )
    .unwrap()
    .to_string()
}

fn fixture_process(test: &str) -> Option<GitFixture> {
    fixture_process_with_files(
        test,
        &[(
            "skills/example/SKILL.md",
            "---\nname: example\ndescription: First revision\n---\nRead A.\n",
        )],
        &[(
            "skills/example/SKILL.md",
            "---\nname: example\ndescription: Second revision\n---\nRead B.\n",
        )],
    )
}

fn fixture_process_with_files(
    test: &str,
    first_files: &[(&str, &str)],
    second_files: &[(&str, &str)],
) -> Option<GitFixture> {
    if let Some(root) = std::env::var_os("WARP_DEFINITION_GIT_FIXTURE") {
        let root = PathBuf::from(root);
        let source = Repository::open(root.join("source")).unwrap();
        let second = source.head().unwrap().peel_to_commit().unwrap();
        return Some(GitFixture {
            working: root.join("working"),
            first: second.parent_id(0).unwrap().to_string(),
            second: second.id().to_string(),
        });
    }
    let root = tempfile::tempdir().unwrap();
    let source = Repository::init(root.path().join("source")).unwrap();
    commit_files(&source, first_files);
    commit_files(&source, second_files);
    Repository::clone(
        source.workdir().unwrap().to_str().unwrap(),
        root.path().join("working"),
    )
    .unwrap();
    let source_url = url::Url::from_directory_path(source.workdir().unwrap()).unwrap();
    let output = BlockingCommand::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("ai::factory_definitions::tests::{test}"),
            "--nocapture",
        ])
        .env("WARP_DEFINITION_GIT_FIXTURE", root.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.path().join("empty-config"))
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", format!("url.{source_url}.insteadOf"))
        .env("GIT_CONFIG_VALUE_0", "https://fixture.invalid/definitions")
        .env("GIT_CONFIG_KEY_1", "protocol.file.allow")
        .env("GIT_CONFIG_VALUE_1", "always")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    None
}

fn metadata(sha: &str) -> FactoryBranchExecutionConfig {
    FactoryBranchExecutionConfig {
        factory_id: cynic::Id::new("factory"),
        branch_id: cynic::Id::new("preview-one"),
        branch: "preview".into(),
        source_sha: sha.into(),
        repository_url: "https://fixture.invalid/definitions".into(),
    }
}

async fn assert_skill_layout_activates(
    test: &str,
    files: &[(&str, &str)],
    expected_names: &[&str],
) {
    let Some(fixture) = fixture_process_with_files(test, files, files) else {
        return;
    };
    let source = metadata(&fixture.second);
    let definitions = prepare_from_config(
        &fixture.working,
        "42",
        Some(&source),
        &["agents/reviewer/skills".into(), "skills".into()],
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(
        definitions
            .skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>(),
        expected_names
    );
}

#[tokio::test]
async fn factory_branch_native_git_activates_without_skill_roots() {
    assert_skill_layout_activates(
        "factory_branch_native_git_activates_without_skill_roots",
        &[("README.md", "Factory definitions without skills.")],
        &[],
    )
    .await;
}

#[tokio::test]
async fn factory_branch_native_git_activates_factory_only_skills() {
    assert_skill_layout_activates(
        "factory_branch_native_git_activates_factory_only_skills",
        &[(
            "skills/example/SKILL.md",
            "---\nname: example\ndescription: Factory skill\n---\nRead selected assets.\n",
        )],
        &["example"],
    )
    .await;
}

#[tokio::test]
async fn factory_branch_native_git_activates_agent_only_skills() {
    assert_skill_layout_activates(
        "factory_branch_native_git_activates_agent_only_skills",
        &[(
            "agents/reviewer/skills/example/SKILL.md",
            "---\nname: example\ndescription: Agent skill\n---\nRead selected assets.\n",
        )],
        &["example"],
    )
    .await;
}

#[tokio::test]
async fn factory_branch_native_git_ignores_ordinary_skill_root_files() {
    assert_skill_layout_activates(
        "factory_branch_native_git_ignores_ordinary_skill_root_files",
        &[
            (
                "agents/reviewer/skills/agent/SKILL.md",
                "---\nname: agent\ndescription: Agent skill\n---\nRead selected assets.\n",
            ),
            (
                "skills/factory/SKILL.md",
                "---\nname: factory\ndescription: Factory skill\n---\nRead selected assets.\n",
            ),
            ("skills/README.md", "Skill documentation."),
        ],
        &["agent", "factory"],
    )
    .await;
}

#[tokio::test]
async fn factory_branch_native_git_preserves_concurrent_immutable_leaves() {
    let Some(fixture) =
        fixture_process("factory_branch_native_git_preserves_concurrent_immutable_leaves")
    else {
        return;
    };
    let original = Repository::open(&fixture.working).unwrap();
    let origin = original
        .find_remote("origin")
        .unwrap()
        .url()
        .unwrap()
        .to_owned();
    let first = metadata(&fixture.first);
    let second = metadata(&fixture.second);
    let skill_dirs = ["skills".into()];
    let (a, same_a, b) = futures::join!(
        prepare_from_config(&fixture.working, "42", Some(&first), &skill_dirs),
        prepare_from_config(&fixture.working, "42", Some(&first), &skill_dirs),
        prepare_from_config(&fixture.working, "42", Some(&second), &skill_dirs),
    );
    let a = a.unwrap().unwrap();
    let same_a = same_a.unwrap().unwrap();
    let b = b.unwrap().unwrap();

    assert_ne!(a.root, same_a.root);
    assert_ne!(a.root, b.root);
    assert_eq!(
        Repository::open(&a.root)
            .unwrap()
            .head()
            .unwrap()
            .target()
            .unwrap()
            .to_string(),
        fixture.first
    );
    assert_eq!(
        Repository::open(&b.root)
            .unwrap()
            .head()
            .unwrap()
            .target()
            .unwrap()
            .to_string(),
        fixture.second
    );
    assert!(
        std::fs::read_to_string(a.skill_dirs[0].join("example/SKILL.md"))
            .unwrap()
            .contains("Read A.")
    );
    assert!(
        std::fs::read_to_string(b.skill_dirs[0].join("example/SKILL.md"))
            .unwrap()
            .contains("Read B.")
    );
    assert_eq!(
        original.head().unwrap().target().unwrap().to_string(),
        fixture.second
    );
    assert_eq!(
        original.find_remote("origin").unwrap().url(),
        Some(origin.as_str())
    );
    assert_eq!(
        Repository::open(&a.root)
            .unwrap()
            .find_remote("origin")
            .unwrap()
            .url(),
        Some("https://fixture.invalid/definitions")
    );
    assert!(
        !fixture
            .working
            .join(".warp/factory-definitions/current")
            .exists()
    );
    assert_eq!(
        a.env_vars().get(&OsString::from("WARP_EXECUTION_ID")),
        Some(&OsString::from("42"))
    );
    assert_eq!(
        a.env_vars().get(&OsString::from("WARP_FACTORY_CONFIG_SHA")),
        Some(&OsString::from(&fixture.first))
    );
    assert_eq!(
        b.env_vars().get(&OsString::from("WARP_FACTORY_CONFIG_SHA")),
        Some(&OsString::from(&fixture.second))
    );
    let mut input = [super::super::agent::AIAgentInput::ResumeConversation {
        context: Arc::from([]),
    }];
    b.add_skill_context(&mut input);
    let super::super::agent::AIAgentContext::Skills { skills } = &input[0].context().unwrap()[0]
    else {
        panic!("missing selected skills");
    };
    let mut stale = skills.clone();
    let mut removed = stale[0].clone();
    removed.name = "removed-from-selected-source".to_owned();
    stale.push(removed);
    *input[0].context_mut().unwrap() =
        vec![super::super::agent::AIAgentContext::Skills { skills: stale }].into();
    a.add_skill_context(&mut input);
    let super::super::agent::AIAgentContext::Skills { skills } = &input[0].context().unwrap()[0]
    else {
        panic!("missing selected skills");
    };
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "example");
    assert_eq!(skills[0].description, "First revision");
    let spec = "example".parse().unwrap();
    assert_eq!(a.resolve_skill(&spec).unwrap().instructions, "Read A.");
    let selected_path = a.root.join("skills/example/SKILL.md");
    let selected_spec = selected_path.to_string_lossy().parse().unwrap();
    assert!(check_authored_skill_path(&selected_spec, &fixture.working, Some(&a)).is_ok());
    assert!(check_authored_skill_path(&selected_spec, &fixture.working, Some(&b)).is_err());
    assert!(check_authored_skill_path(&selected_spec, &fixture.working, None).is_err());
}

#[test]
fn factory_branch_selected_skill_order_prefers_agent_over_factory() {
    let temp = tempfile::tempdir().unwrap();
    let agent = temp.path().join("agent");
    let factory = temp.path().join("factory");
    for (root, description) in [
        (&agent, "Agent definitions"),
        (&factory, "Factory definitions"),
    ] {
        let skill = root.join("example/SKILL.md");
        std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
        std::fs::write(
            skill,
            format!("---\nname: example\ndescription: {description}\n---\nRead selected assets.\n"),
        )
        .unwrap();
    }
    let skills = read_selected_skills(&[agent.clone(), factory]).unwrap();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].description, "Agent definitions");
    assert_eq!(
        skills[0].path.to_local_path(),
        Some(agent.join("example/SKILL.md").as_path())
    );
}

#[test]
fn factory_branch_invalid_selected_skill_cannot_fall_back_to_an_ordinary_name() {
    let dir = tempfile::tempdir().unwrap();
    let skill = dir.path().join("example/SKILL.md");
    std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
    std::fs::write(&skill, [0xff, 0xfe]).unwrap();

    assert!(read_selected_skills(&[dir.path().to_path_buf()]).is_err());
}

#[test]
fn factory_branch_existing_non_directory_skill_root_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let skills = root.path().join("skills");
    std::fs::write(&skills, "Not a skill directory.").unwrap();

    assert!(read_selected_skills(&[skills]).is_err());
}

#[cfg(unix)]
#[test]
fn factory_branch_absent_skill_root_does_not_hide_a_dangling_ancestor() {
    let root = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink("missing-agent", root.path().join("agent")).unwrap();

    assert!(read_selected_skills(&[root.path().join("agent/skills")]).is_err());
}

#[cfg(unix)]
#[test]
fn factory_branch_absent_optional_roots_do_not_bypass_symlink_containment() {
    let outside = tempfile::tempdir().unwrap();
    for target in [Path::new("missing-agent"), outside.path()] {
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(target, root.path().join("agent")).unwrap();
        let definitions = ActivatedDefinitions {
            execution_id: "42".into(),
            metadata: metadata("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            root: dunce::canonicalize(root.path()).unwrap(),
            skill_dirs: vec![root.path().join("agent/skills")],
            skills: Vec::new(),
        };

        assert!(
            validate_checkout(&definitions).is_err(),
            "Accepted an absent skill root through {}",
            target.display()
        );
    }
}

#[cfg(unix)]
#[test]
fn factory_branch_contained_directory_symlinks_remain_discoverable() {
    let root = tempfile::tempdir().unwrap();
    let skill = root.path().join("assets/example/SKILL.md");
    std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
    std::fs::write(
        skill,
        "---\nname: example\ndescription: Contained skill\n---\nRead selected assets.\n",
    )
    .unwrap();
    std::fs::create_dir(root.path().join("skills")).unwrap();
    std::os::unix::fs::symlink("../assets/example", root.path().join("skills/linked")).unwrap();
    std::os::unix::fs::symlink("skills", root.path().join("skills-alias")).unwrap();
    let definitions = ActivatedDefinitions {
        execution_id: "42".into(),
        metadata: metadata("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        root: dunce::canonicalize(root.path()).unwrap(),
        skill_dirs: vec![root.path().join("skills-alias")],
        skills: Vec::new(),
    };

    validate_checkout(&definitions).unwrap();
    let skills = read_selected_skills(&definitions.skill_dirs).unwrap();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "example");
}

#[test]
fn factory_branch_git_diagnostics_redact_encoded_credentials_before_truncation() {
    let credential = GitCredential {
        host: "fixture.invalid".into(),
        token: "fixture:/password+%é".into(),
        username: None,
        email: None,
    };
    let mut serialized = url::Url::parse("https://fixture.invalid").unwrap();
    serialized.set_password(Some(&credential.token)).unwrap();
    let escaped = serialized.password().unwrap();
    let url_encoded = urlencoding::encode(&credential.token);
    let diagnostic = format!(
        "{} {} {} {}",
        credential.token,
        escaped,
        url_encoded,
        "x".repeat(20000)
    );

    let sanitized = git_failure_diagnostic(diagnostic.as_bytes(), Some(&credential)).unwrap();
    assert!(!sanitized.contains(&credential.token));
    assert!(!sanitized.contains(escaped));
    assert!(!sanitized.contains(&*url_encoded));
    assert!(sanitized.starts_with("[REDACTED] [REDACTED] [REDACTED]"));
    assert!(sanitized.contains("git output truncated"));
}

#[test]
fn factory_branch_rejects_unsafe_source_paths() {
    let valid = metadata("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    assert!(validate_metadata(&valid, &["../skills".into()]).is_err());
    assert!(validate_metadata(&valid, &["C:\\skills".into()]).is_err());
    let mut credential_url = valid.clone();
    credential_url.repository_url = "https://user:password@fixture.invalid/definitions".into();
    assert!(validate_metadata(&credential_url, &[]).is_err());
    let mut branch = valid;
    branch.branch_id = cynic::Id::new("../other");
    assert!(validate_metadata(&branch, &[]).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn factory_branch_rejects_symlinked_definition_parent() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join(".warp")).unwrap();

    assert!(
        materialize(
            dir.path(),
            "42",
            &metadata("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            &[],
            None
        )
        .await
        .is_err()
    );
    assert!(!outside.path().join("factory-definitions").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn factory_branch_reuses_installed_repository_helper_without_modifying_it() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    let helper = "!f() { printf 'username=fixture\\npassword=fixture-password\\n'; }; f";
    repo.config()
        .unwrap()
        .set_str("credential.helper", helper)
        .unwrap();
    let metadata = metadata("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let credential = installed_credential(dir.path(), &metadata)
        .await
        .unwrap()
        .unwrap();
    let protected = temporary_credential(&credential, &metadata).unwrap();
    let temporary_path = protected.path().to_owned();

    assert_eq!(credential.host, "fixture.invalid");
    assert_eq!(credential.token, "fixture-password");
    assert_eq!(
        repo.config()
            .unwrap()
            .get_string("credential.helper")
            .unwrap(),
        helper
    );
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(
        protected.as_file().metadata().unwrap().permissions().mode() & 0o777,
        0o600
    );
    drop(protected);
    assert!(!temporary_path.exists());
}
