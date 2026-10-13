use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, bail};
use command::Stdio;
use command::r#async::Command;
use futures::AsyncWriteExt as _;
use uuid::Uuid;
use warp_graphql::queries::execution_config::FactoryDefinitionConfig;

use crate::server::server_api::ai::GitCredential;
pub(crate) async fn prepare_child(
    task_id: &str,
    working_dir: &Path,
    factory_preview: bool,
    client: &dyn crate::server::server_api::ai::AIClient,
) -> anyhow::Result<Option<ActivatedDefinitions>> {
    if !factory_preview {
        return Ok(None);
    }
    let config = client
        .get_factory_definition_config(task_id)
        .await?
        .context("Factory child execution configuration has no selected definitions")?;
    prepare_from_config(task_id, working_dir, Some(&config)).await
}

const SETUP_BUDGET: Duration = Duration::from_secs(30);
pub(crate) async fn prepare_from_config(
    task_id: &str,
    working_dir: &Path,
    config: Option<&warp_graphql::queries::execution_config::FactoryDefinitionConfig>,
) -> anyhow::Result<Option<ActivatedDefinitions>> {
    let Some(config) = config else {
        return Ok(None);
    };
    let metadata = config.clone();
    validate_metadata(&metadata)?;
    async_compat::Compat::new(async {
        tokio::time::timeout(SETUP_BUDGET, async {
            let credential =
                match super::agent_sdk::driver::git_credentials::definition_credential(task_id) {
                    Some(credential) => Some(credential),
                    None => installed_credential(working_dir, &metadata).await?,
                };
            let mut definitions = materialize(working_dir, &metadata, credential.as_ref()).await?;
            definitions.skills = read_selected_skills(&definitions.skill_dirs)?;
            Ok(Some(definitions))
        })
        .await
        .context("Factory definition checkout timed out")?
    })
    .await
}

#[derive(Clone, Debug)]
pub struct ActivatedDefinitions {
    pub metadata: FactoryDefinitionConfig,
    pub root: PathBuf,
    pub skill_dirs: Vec<PathBuf>,
    pub(crate) skills: Vec<ai::skills::ParsedSkill>,
}

impl ActivatedDefinitions {
    pub(crate) fn resolve_skill(
        &self,
        spec: &warp_cli::skill::SkillSpec,
    ) -> Option<crate::ai::skills::ResolvedSkill> {
        if spec.repo.is_some() || spec.is_full_path() {
            return None;
        }
        self.skills
            .iter()
            .find(|skill| skill.name == spec.skill_identifier)
            .cloned()
            .and_then(|skill| {
                let path = skill.path.to_local_path()?.to_path_buf();
                Some(crate::ai::skills::to_resolved_skill(path, skill))
            })
    }
    pub(crate) fn add_skill_context(&self, inputs: &mut [super::agent::AIAgentInput]) {
        use ai::skills::{SkillDescriptor, SkillReference};

        use super::agent::AIAgentContext;

        let mut names = HashSet::new();
        let selected: Vec<SkillDescriptor> = self
            .skills
            .iter()
            .filter(|skill| names.insert(skill.name.clone()))
            .cloned()
            .map(Into::into)
            .collect();
        for input in inputs {
            let Some(context) = input.context_mut() else {
                continue;
            };
            let mut context_vec = context.to_vec();
            let mut skills = selected.clone();
            for entry in &context_vec {
                if let AIAgentContext::Skills { skills: existing } = entry {
                    skills.extend(
                        existing
                            .iter()
                            .filter(|skill| {
                                !names.contains(&skill.name)
                                    && !matches!(&skill.reference, SkillReference::Path(path)
                                if path.to_local_path().is_some_and(is_definition_path))
                            })
                            .cloned(),
                    );
                }
            }
            context_vec.retain(|entry| !matches!(entry, AIAgentContext::Skills { .. }));
            context_vec.push(AIAgentContext::Skills { skills });
            *context = context_vec.into();
        }
    }
    pub fn env_vars(&self) -> HashMap<OsString, OsString> {
        HashMap::from([
            (
                "WARP_FACTORY_ID".into(),
                self.metadata.factory_id.inner().into(),
            ),
            (
                "WARP_FACTORY_BRANCH".into(),
                self.metadata.branch.clone().into(),
            ),
            (
                "WARP_FACTORY_CONFIG_SHA".into(),
                self.metadata.source_sha.clone().into(),
            ),
            (
                ai::skills::WARP_SKILL_DIRS_ENV.into(),
                self.skill_dirs
                    .iter()
                    .map(|dir| dir.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(",")
                    .into(),
            ),
        ])
    }
}

pub(crate) fn is_definition_path(path: &Path) -> bool {
    let canonical = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical
        .components()
        .collect::<Vec<_>>()
        .windows(2)
        .any(|parts| {
            parts[0].as_os_str() == ".warp" && parts[1].as_os_str() == "factory-definitions"
        })
}

pub(crate) fn check_authored_skill_path(
    spec: &warp_cli::skill::SkillSpec,
    working_dir: &Path,
    definitions: Option<&ActivatedDefinitions>,
) -> anyhow::Result<()> {
    if !spec.is_full_path() {
        return Ok(());
    }
    let path = working_dir.join(&spec.skill_identifier);
    let path = dunce::canonicalize(&path).unwrap_or(path);
    if is_definition_path(&path)
        && !definitions.is_some_and(|definitions| path.starts_with(&definitions.root))
    {
        bail!("Downloaded factory skills require activation for the selected task");
    }
    Ok(())
}

fn read_selected_skills(dirs: &[PathBuf]) -> anyhow::Result<Vec<ai::skills::ParsedSkill>> {
    let mut skills = Vec::new();
    let mut names = HashSet::new();
    for dir in dirs {
        if !validate_optional_skill_directory(dir)? {
            continue;
        }
        let mut paths = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if std::fs::metadata(&path)?.is_dir() {
                paths.push(path.join("SKILL.md"));
            }
        }
        paths.sort();
        for path in paths {
            if !path.try_exists()? {
                continue;
            }
            let mut skill = ai::skills::parse_skill(&path).map_err(|_| {
                anyhow::anyhow!("Could not parse selected factory skill {}", path.display())
            })?;
            if names.insert(skill.name.clone()) {
                skill.scope = ai::skills::SkillScope::Home;
                skills.push(skill);
            }
        }
    }
    Ok(skills)
}

fn validate_optional_skill_directory(path: &Path) -> anyhow::Result<bool> {
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => {
                if !std::fs::metadata(ancestor)?.is_dir() {
                    bail!("Factory skill directory path contains a non-directory");
                }
                return Ok(ancestor == path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    bail!("Factory skill directory has no existing ancestor")
}

fn validate_metadata(metadata: &FactoryDefinitionConfig) -> anyhow::Result<()> {
    let branch = metadata.branch_id.inner();
    if branch.is_empty()
        || !branch
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        || metadata.source_sha.len() != 40
        || !metadata
            .source_sha
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("Invalid factory definition branch identity or full source SHA");
    }
    let url = url::Url::parse(&metadata.repository_url)
        .context("Invalid factory definition repository URL")?;
    if url.scheme() != "https"
        || metadata.repository_url.chars().any(char::is_control)
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("Factory definition repository URL must be credential-free HTTPS");
    }
    for dir in &metadata.skill_dirs {
        if dir.is_empty()
            || dir.trim() != dir
            || dir.contains([',', '\\', '\0', ':'])
            || Path::new(dir)
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            bail!("Factory skill directory must be an unambiguous repository-relative path");
        }
    }
    Ok(())
}

fn real_directory(path: &Path) -> anyhow::Result<()> {
    match std::fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("Factory definition path must not contain symlinks or non-directories");
    }
    if dunce::canonicalize(path)? != path {
        bail!("Factory definition directory ancestor was replaced by a symlink");
    }
    Ok(())
}

async fn materialize(
    working_dir: &Path,
    metadata: &FactoryDefinitionConfig,
    credential: Option<&GitCredential>,
) -> anyhow::Result<ActivatedDefinitions> {
    validate_metadata(metadata)?;
    let working_dir = dunce::canonicalize(working_dir)?;
    if working_dir.to_string_lossy().contains(',') {
        bail!("Factory definition working directory cannot contain a comma");
    }
    let mut parent = working_dir.clone();
    for part in [
        ".warp",
        "factory-definitions",
        metadata.branch_id.inner(),
        &metadata.source_sha,
    ] {
        parent.push(part);
        real_directory(&parent)?;
    }
    let root = parent.join(Uuid::new_v4().to_string());
    std::fs::create_dir(&root)?;
    let auth_file = credential
        .map(|credential| temporary_credential(credential, metadata))
        .transpose()?;
    let auth = auth_file.as_ref().map(|file| file.path());
    git(&root, &["init", "--quiet", "--template="], auth, credential).await?;
    git(
        &root,
        &["remote", "add", "origin", &metadata.repository_url],
        auth,
        credential,
    )
    .await?;
    git(
        &root,
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            "--depth=1",
            "origin",
            &metadata.source_sha,
        ],
        auth,
        credential,
    )
    .await
    .context("Could not read factory definitions with existing runtime Git authentication")?;
    git(
        &root,
        &["checkout", "--quiet", "--detach", &metadata.source_sha],
        auth,
        credential,
    )
    .await?;
    let head = git(&root, &["rev-parse", "HEAD"], auth, credential).await?;
    if head.trim() != metadata.source_sha {
        bail!("Factory definition checkout does not match the checked source SHA");
    }
    let definitions = ActivatedDefinitions {
        metadata: metadata.clone(),
        skill_dirs: metadata
            .skill_dirs
            .iter()
            .map(|dir| root.join(dir))
            .collect(),
        root,
        skills: Vec::new(),
    };
    validate_checkout(&definitions)?;
    Ok(definitions)
}

fn temporary_credential(
    credential: &GitCredential,
    metadata: &FactoryDefinitionConfig,
) -> anyhow::Result<tempfile::NamedTempFile> {
    let repository = url::Url::parse(&metadata.repository_url)?;
    if repository.host_str() != Some(&credential.host) || credential.token.is_empty() {
        bail!("Normal runtime definition credential does not match the registered source");
    }
    let mut credential_url = repository.clone();
    credential_url
        .set_username(credential.username.as_deref().unwrap_or("x-access-token"))
        .map_err(|_| anyhow::anyhow!("Invalid runtime Git username"))?;
    credential_url
        .set_password(Some(&credential.token))
        .map_err(|_| anyhow::anyhow!("Invalid runtime Git credential"))?;
    let mut file = tempfile::NamedTempFile::new()?;
    writeln!(file, "{credential_url}")?;
    file.flush()?;
    Ok(file)
}

async fn installed_credential(
    working_dir: &Path,
    metadata: &FactoryDefinitionConfig,
) -> anyhow::Result<Option<GitCredential>> {
    let mut child = Command::new("git")
        .current_dir(working_dir)
        .args(["credential", "fill"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("Could not resolve installed Git authentication")?;
    let mut stdin = child.stdin.take().context("Missing Git credential input")?;
    stdin
        .write_all(format!("url={}\n\n", metadata.repository_url).as_bytes())
        .await?;
    drop(stdin);
    let output = child.output().await?;
    if !output.status.success() {
        return Ok(None);
    }
    let output = String::from_utf8(output.stdout)?;
    let fields: HashMap<_, _> = output
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect();
    let Some(token) = fields.get("password").filter(|token| !token.is_empty()) else {
        return Ok(None);
    };
    Ok(Some(GitCredential {
        host: url::Url::parse(&metadata.repository_url)?
            .host_str()
            .context("Missing source host")?
            .to_owned(),
        token: (*token).to_owned(),
        username: fields
            .get("username")
            .map(|username| (*username).to_owned()),
        email: None,
    }))
}

async fn git(
    root: &Path,
    args: &[&str],
    credential_file: Option<&Path>,
    credential: Option<&GitCredential>,
) -> anyhow::Result<String> {
    let hooks = tempfile::tempdir()?;
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .arg("-c")
        .arg(format!("core.hooksPath={}", hooks.path().display()))
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    if let Some(file) = credential_file {
        command
            .args(["-c", "credential.helper=", "-c"])
            .arg(format!(
                "credential.helper=store --file='{}'",
                file.to_string_lossy().replace('\'', "'\\''")
            ))
            .args(["-c", "credential.useHttpPath=true"]);
    }
    command.args(args);
    let output = command
        .output()
        .await
        .context("Could not run native Git for factory definitions")?;
    if !output.status.success() {
        let diagnostic = git_failure_diagnostic(&output.stderr, credential)?;
        bail!("Factory definition Git operation failed: {diagnostic}");
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn git_failure_diagnostic(
    output: &[u8],
    credential: Option<&GitCredential>,
) -> anyhow::Result<String> {
    let mut diagnostic = String::from_utf8_lossy(output).into_owned();
    if let Some(credential) = credential {
        let mut encoded = url::Url::parse("https://t@credential.invalid")?;
        encoded
            .set_password(Some(&credential.token))
            .map_err(|_| anyhow::anyhow!("Invalid runtime Git credential"))?;
        if let Some(password) = encoded.password() {
            diagnostic = diagnostic.replace(password, "[REDACTED]");
        }
        diagnostic = diagnostic.replace(&credential.token, "[REDACTED]");
        diagnostic = diagnostic.replace(
            &urlencoding::encode(&credential.token).into_owned(),
            "[REDACTED]",
        );
    }
    Ok(
        super::agent_sdk::driver::failure_output::prepare_failure_output(
            &diagnostic,
            "\n… git output truncated …\n",
        ),
    )
}

fn validate_checkout(definitions: &ActivatedDefinitions) -> anyhow::Result<()> {
    let root = dunce::canonicalize(&definitions.root)?;
    if root != definitions.root {
        bail!("Factory definition leaf was replaced by a symlink");
    }
    for entry in walkdir::WalkDir::new(&root).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_symlink() {
            let target = dunce::canonicalize(entry.path())
                .context("Factory definition contains a dangling symlink")?;
            if !target.starts_with(&root) {
                bail!("Factory definition symlink escapes its immutable leaf");
            }
        }
    }
    for dir in &definitions.skill_dirs {
        if !validate_optional_skill_directory(dir)? {
            continue;
        }
        if !dunce::canonicalize(dir)?.starts_with(&root) {
            bail!("Factory skill directory escapes its immutable leaf");
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "factory_definitions_tests.rs"]
mod tests;
