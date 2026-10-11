use cynic::QueryBuilder as _;
use serde_json::json;

use super::*;

#[test]
fn factory_branch_root_scope_has_no_selectors() {
    let operation = LocalTaskRootScopeQuery::build(());
    assert_eq!(
        operation.query,
        "query LocalTaskRootScopeQuery {\n  localTaskRootScope\n}\n"
    );
}

#[test]
fn factory_branch_root_scope_rejects_missing_null_and_unknown_values() {
    for value in [
        json!({}),
        json!({"localTaskRootScope": null}),
        json!({"localTaskRootScope": "UNKNOWN"}),
    ] {
        assert!(serde_json::from_value::<LocalTaskRootScopeQuery>(value).is_err());
    }
    assert_eq!(
        serde_json::from_value::<LocalTaskRootScopeQuery>(
            json!({"localTaskRootScope": "PRODUCTION"})
        )
        .unwrap()
        .local_task_root_scope,
        LocalTaskRootScope::Production,
    );
}
