use command::blocking::Command as BlockingCommand;
use git2::{Repository, Signature};

use super::*;
use crate::server::server_api::ai::MockAIClient;

struct GitFixture {
    working: PathBuf,
    first: String,
    second: String,
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

fn metadata(sha: &str) -> FactoryDefinitionSetup {
    FactoryDefinitionSetup {
        factory_id: cynic::Id::new("factory"),
        branch_id: cynic::Id::new("preview-one"),
        branch: "preview".into(),
        source_fingerprint: "registered-source".into(),
        source_sha: sha.into(),
        repository_url: "https://fixture.invalid/definitions".into(),
        skill_dirs: vec!["skills".into()],
        local: true,
    }
}

fn ready_client(metadata: FactoryDefinitionSetup) -> MockAIClient {
    let mut client = MockAIClient::new();
    client
        .expect_get_factory_definition_setup()
        .returning(move |_, _, _| Ok(Some(metadata.clone())));
    client
}

async fn assert_skill_layout_activates(
    test: &str,
    files: &[(&str, &str)],
    expected_names: &[&str],
) {
    let Some(fixture) = fixture_process_with_files(test, files, files) else {
        return;
    };
    let mut source = metadata(&fixture.second);
    source.skill_dirs = vec!["agents/reviewer/skills".into(), "skills".into()];
    let definitions = prepare(
        "layout",
        None,
        None,
        &fixture.working,
        &ready_client(source),
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
    let first = ready_client(metadata(&fixture.first));
    let second = ready_client(metadata(&fixture.second));
    let (a, same_a, b) = futures::join!(
        prepare("a", None, None, &fixture.working, &first),
        prepare("same-a", None, None, &fixture.working, &first),
        prepare("b", None, None, &fixture.working, &second),
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
    remove_definition_skill_context(&mut input);
    let super::super::agent::AIAgentContext::Skills { skills } = &input[0].context().unwrap()[0]
    else {
        panic!("missing skill context");
    };
    assert!(skills.is_empty());
    let spec = "example".parse().unwrap();
    assert_eq!(a.resolve_skill(&spec).unwrap().instructions, "Read A.");
    let selected_path = a.root.join("skills/example/SKILL.md");
    let selected_spec = selected_path.to_string_lossy().parse().unwrap();
    assert!(check_authored_skill_path(&selected_spec, &fixture.working, Some(&a)).is_ok());
    assert!(check_authored_skill_path(&selected_spec, &fixture.working, Some(&b)).is_err());
    assert!(check_authored_skill_path(&selected_spec, &fixture.working, None).is_err());

    let session = DefinitionSession::loaded("a".into(), Some(a.clone()));
    let unavailable = MockAIClient::new();
    let retained = session
        .prepare("a", None, &unavailable)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retained.root, a.root);
}

#[tokio::test]
async fn factory_branch_native_git_rebuilds_a_changed_activation_sha() {
    let Some(fixture) =
        fixture_process("factory_branch_native_git_rebuilds_a_changed_activation_sha")
    else {
        return;
    };
    let initial = metadata(&fixture.first);
    let current = metadata(&fixture.second);
    let mut client = MockAIClient::new();
    client
        .expect_get_factory_definition_setup()
        .returning(move |_, input, _| match input.phase {
            FactoryDefinitionSetupPhase::Metadata => Ok(Some(initial.clone())),
            FactoryDefinitionSetupPhase::Materialize => {
                if input.source_sha.as_deref() == Some(&initial.source_sha) {
                    Ok(Some(initial.clone()))
                } else {
                    Ok(Some(current.clone()))
                }
            }
            FactoryDefinitionSetupPhase::Activate => Ok(Some(current.clone())),
        });
    let definitions = prepare("moving", None, None, &fixture.working, &client)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(definitions.metadata.source_sha, fixture.second);
    assert!(
        std::fs::read_to_string(definitions.skill_dirs[0].join("example/SKILL.md"))
            .unwrap()
            .contains("Read B.")
    );
    let initial_leaves = fixture
        .working
        .join(".warp/factory-definitions/preview-one")
        .join(&fixture.first);
    assert_eq!(std::fs::read_dir(initial_leaves).unwrap().count(), 1);
}

#[tokio::test]
async fn factory_branch_denied_activation_never_loads_downloaded_skills() {
    let Some(fixture) =
        fixture_process("factory_branch_denied_activation_never_loads_downloaded_skills")
    else {
        return;
    };
    let source = metadata(&fixture.first);
    let mut client = MockAIClient::new();
    client
        .expect_get_factory_definition_setup()
        .returning(move |_, input, _| match input.phase {
            FactoryDefinitionSetupPhase::Metadata | FactoryDefinitionSetupPhase::Materialize => {
                Ok(Some(source.clone()))
            }
            FactoryDefinitionSetupPhase::Activate => {
                Err(anyhow::anyhow!("source permission revoked"))
            }
        });
    let result = prepare("revoked", None, None, &fixture.working, &client).await;

    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("source permission revoked")
    );
    assert_eq!(
        Repository::open(&fixture.working)
            .unwrap()
            .head()
            .unwrap()
            .target()
            .unwrap()
            .to_string(),
        fixture.second
    );
}

#[tokio::test]
async fn factory_branch_source_identity_changes_fail_before_git() {
    let dir = tempfile::tempdir().unwrap();
    let original = metadata("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let mut changed = original.clone();
    changed.source_fingerprint = "replacement".into();
    let mut client = MockAIClient::new();
    client
        .expect_get_factory_definition_setup()
        .returning(move |_, input, _| match input.phase {
            FactoryDefinitionSetupPhase::Metadata => Ok(Some(original.clone())),
            FactoryDefinitionSetupPhase::Materialize | FactoryDefinitionSetupPhase::Activate => {
                Ok(Some(changed.clone()))
            }
        });

    assert!(
        prepare("changed", None, None, dir.path(), &client)
            .await
            .is_err()
    );
    assert!(!dir.path().join(".warp").exists());
}

#[tokio::test]
async fn factory_branch_materialization_stops_after_three_distinct_heads() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = MockAIClient::new();
    client
        .expect_get_factory_definition_setup()
        .times(4)
        .returning(|_, input, _| {
            let next = match input.source_sha.as_deref() {
                None => 'a',
                Some(sha) if sha.starts_with('a') => 'b',
                Some(sha) if sha.starts_with('b') => 'c',
                Some(sha) if sha.starts_with('c') => 'd',
                Some(sha) => panic!("unexpected head {sha}"),
            };
            assert_ne!(input.phase, FactoryDefinitionSetupPhase::Activate);
            Ok(Some(metadata(&next.to_string().repeat(40))))
        });

    let error = prepare("moving", None, None, dir.path(), &client)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("head moved"));
    assert!(!dir.path().join(".warp").exists());
}

#[tokio::test]
async fn factory_branch_expired_metadata_cannot_materialize() {
    let dir = tempfile::tempdir().unwrap();
    let preparation = DefinitionPreparation {
        metadata: Some(metadata("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")),
        deadline: Instant::now() - Duration::from_secs(1),
    };
    let client = MockAIClient::new();

    assert!(
        preparation
            .activate("expired", None, None, dir.path(), &client)
            .await
            .is_err()
    );
    assert!(!dir.path().join(".warp").exists());
}

#[tokio::test]
async fn factory_branch_production_does_not_materialize() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = MockAIClient::new();
    client
        .expect_get_factory_definition_setup()
        .returning(|_, _, _| Ok(None));

    assert!(
        prepare("production", None, None, dir.path(), &client)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        DefinitionSession::default()
            .prepare("production", None, &client)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!dir.path().join(".warp").exists());
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
            metadata: metadata("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            root: dunce::canonicalize(root.path()).unwrap(),
            skill_dirs: vec![root.path().join("agent/skills")],
            skills: Vec::new(),
            setup_deadline: Instant::now() + SETUP_BUDGET,
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
        metadata: metadata("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        root: dunce::canonicalize(root.path()).unwrap(),
        skill_dirs: vec![root.path().join("skills-alias")],
        skills: Vec::new(),
        setup_deadline: Instant::now() + SETUP_BUDGET,
    };

    validate_checkout(&definitions).unwrap();
    let skills = read_selected_skills(&definitions.skill_dirs).unwrap();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "example");
}

#[tokio::test]
async fn factory_branch_root_admission_requires_a_fresh_positive_production_result() {
    use warp_graphql::queries::local_task_root_scope::LocalTaskRootScope;
    let mut client = MockAIClient::new();
    let mut sequence = mockall::Sequence::new();
    client
        .expect_get_local_task_root_scope()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|| Ok(LocalTaskRootScope::Production));
    client
        .expect_get_local_task_root_scope()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|| Ok(LocalTaskRootScope::Preview));
    client
        .expect_get_local_task_root_scope()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|| Ok(LocalTaskRootScope::Unavailable));
    client
        .expect_get_local_task_root_scope()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|| Err(anyhow::anyhow!("scope store unavailable")));

    assert!(require_production_root(&client).await.is_ok());
    assert!(require_production_root(&client).await.is_err());
    assert!(require_production_root(&client).await.is_err());
    assert!(
        require_production_root(&client)
            .await
            .unwrap_err()
            .to_string()
            .contains("scope store unavailable")
    );
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
    let mut traversal = valid.clone();
    traversal.skill_dirs = vec!["../skills".into()];
    assert!(validate_metadata(&traversal).is_err());
    let mut absolute = valid.clone();
    absolute.skill_dirs = vec!["C:\\skills".into()];
    assert!(validate_metadata(&absolute).is_err());
    let mut credential_url = valid.clone();
    credential_url.repository_url = "https://user:password@fixture.invalid/definitions".into();
    assert!(validate_metadata(&credential_url).is_err());
    let mut branch = valid;
    branch.branch_id = cynic::Id::new("../other");
    assert!(validate_metadata(&branch).is_err());
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
            &metadata("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
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
