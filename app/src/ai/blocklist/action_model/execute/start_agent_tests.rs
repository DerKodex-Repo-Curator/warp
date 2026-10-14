use warpui::App;

use super::*;
use crate::workspaces::user_workspaces::TeamlessScopeForTest;

#[test]
fn factory_branch_local_dispatch_fails_before_child_creation() {
    App::test((), |mut app| async move {
        app.add_singleton_model(|_| BlocklistAIHistoryModel::new(vec![], vec![], &[]));
        let executor = app.add_model(StartAgentExecutor::new);
        let outcome = executor.update(&mut app, |executor, ctx| {
            executor.set_factory_branch_active(true);
            let receiver = executor.dispatch(
                "child".into(),
                "prompt".into(),
                StartAgentExecutionMode::Local {
                    harness_type: None,
                    model_id: None,
                },
                HashMap::new(),
                None,
                AIConversationId::new(),
                None,
                RequestTeamScope::from_scope(&TeamlessScopeForTest),
                ctx,
            );
            assert!(executor.pending.is_empty());
            receiver.try_recv().unwrap()
        });
        assert!(matches!(
            outcome,
            StartAgentOutcome::Error(message)
                if message.contains("headless AgentDriver launch or remote execution")
        ));
    });
}
