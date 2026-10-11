use crate::schema;

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "RootQuery")]
pub struct LocalTaskRootScopeQuery {
    pub local_task_root_scope: LocalTaskRootScope,
}
crate::client::define_operation! {
    local_task_root_scope(()) -> LocalTaskRootScopeQuery;
}

#[derive(cynic::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalTaskRootScope {
    #[cynic(rename = "PRODUCTION")]
    Production,
    #[cynic(rename = "PREVIEW")]
    Preview,
    #[cynic(rename = "UNAVAILABLE")]
    Unavailable,
}

#[cfg(test)]
#[path = "local_task_root_scope_tests.rs"]
mod tests;
