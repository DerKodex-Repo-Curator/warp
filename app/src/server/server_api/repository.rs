use cloud_object_models::{CodeForge, SourceRepo};
use serde::Deserialize;
use warp_cli::agent::RepositoryHeadRef;

/// Defines a server-selected checkout for normal run setup and standby preparation.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ResolvedRepository {
    pub forge: CodeForge,
    pub owner: String,
    pub name: String,
    #[serde(default, rename = "ref")]
    pub checkout: Option<RepositoryHeadRef>,
    /// Overrides the clone source without changing the repository identity or checkout directory.
    #[serde(default)]
    pub clone_from: Option<SourceRepo>,
    /// Keeps the origin remote that sandbox preparation would otherwise remove.
    #[serde(default)]
    pub preserve_origin: bool,
}

impl ResolvedRepository {
    /// Returns the logical repository identity rather than an alternate clone source.
    #[cfg_attr(target_family = "wasm", expect(dead_code))]
    pub(crate) fn source_repo(&self) -> SourceRepo {
        SourceRepo::new(self.forge, self.owner.clone(), self.name.clone())
    }
}
