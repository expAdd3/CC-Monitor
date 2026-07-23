use monitor_domain::{
    AgentKind, Confidence, EventSource, SessionId, SessionLifecycle, SessionProjection,
    StateReason, TurnState,
};
use monitor_storage::ProjectionKey;
use std::collections::HashMap;

fn projection(agent: &str) -> SessionProjection {
    SessionProjection {
        agent_kind: AgentKind::from(agent),
        session_id: SessionId::from("shared-session"),
        lifecycle: SessionLifecycle::Active,
        turn_state: TurnState::Running,
        reason: StateReason::UserPromptSubmitted,
        source: EventSource::Hook,
        confidence: Confidence::Definitive,
        revision: 1,
        changed_at_ms: 1,
        last_observed_at_ms: 1,
        current_turn_id: None,
    }
}

#[test]
fn projection_key_isolates_equal_session_ids_between_agents() {
    let claude = projection("claude");
    let future_agent = projection("future-agent");
    let mut projections = HashMap::new();
    projections.insert(ProjectionKey::from(&claude), claude);
    projections.insert(ProjectionKey::from(&future_agent), future_agent);

    assert_eq!(projections.len(), 2);
    assert_eq!(
        projections
            .get(&ProjectionKey {
                agent_kind: AgentKind::from("claude"),
                session_id: SessionId::from("shared-session"),
            })
            .unwrap()
            .agent_kind,
        AgentKind::claude()
    );
}
