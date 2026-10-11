use super::task_attachments::TaskInput;
use crate::error::UserFacingError;
use crate::request_context::RequestContext;
use crate::schema;

pub const CAPABILITY_HEADER: &str = "X-Warp-Factory-Definition-Setup";

#[derive(cynic::QueryVariables, Debug)]
pub struct FactoryDefinitionVariables {
    pub task_input: TaskInput,
    pub setup_input: FactoryDefinitionSetupInput,
    pub request_context: RequestContext,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "RootQuery", variables = "FactoryDefinitionVariables")]
pub struct FactoryDefinitionQuery {
    #[arguments(input: $task_input, requestContext: $request_context)]
    pub task: FactoryDefinitionTaskResult,
}

crate::client::define_operation! {
    factory_definition_setup(FactoryDefinitionVariables) -> FactoryDefinitionQuery;
}

#[derive(cynic::InlineFragments, Debug)]
#[cynic(graphql_type = "TaskResult", variables = "FactoryDefinitionVariables")]
pub enum FactoryDefinitionTaskResult {
    TaskOutput(FactoryDefinitionTaskOutput),
    UserFacingError(UserFacingError),
    #[cynic(fallback)]
    Unknown,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "TaskOutput", variables = "FactoryDefinitionVariables")]
pub struct FactoryDefinitionTaskOutput {
    pub task: FactoryDefinitionTask,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Task", variables = "FactoryDefinitionVariables")]
pub struct FactoryDefinitionTask {
    #[arguments(input: $setup_input)]
    pub factory_definition_setup: Option<FactoryDefinitionSetup>,
}

#[derive(cynic::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FactoryDefinitionSetupPhase {
    #[cynic(rename = "METADATA")]
    Metadata,
    #[cynic(rename = "MATERIALIZE")]
    Materialize,
    #[cynic(rename = "ACTIVATE")]
    Activate,
}

#[derive(cynic::InputObject, Debug)]
pub struct FactoryDefinitionSetupInput {
    pub phase: FactoryDefinitionSetupPhase,
    pub execution_id: Option<cynic::Id>,
    pub branch_id: Option<cynic::Id>,
    pub source_fingerprint: Option<String>,
    pub source_sha: Option<String>,
}

#[derive(cynic::QueryFragment, Clone, Debug, PartialEq, Eq)]
pub struct FactoryDefinitionSetup {
    pub factory_id: cynic::Id,
    pub branch_id: cynic::Id,
    pub branch: String,
    pub source_fingerprint: String,
    pub source_sha: String,
    pub repository_url: String,
    pub skill_dirs: Vec<String>,
    pub local: bool,
}
