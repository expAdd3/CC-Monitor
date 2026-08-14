use crate::{
    AgentEvent, Confidence, EventSource, NotificationEdge, NotificationKind, Reduction,
    SessionLifecycle, SessionProjection, StateReason, TurnState,
};
use std::collections::HashSet;

#[derive(Default)]
struct ReducerState {
    projection: Option<SessionProjection>,
    unresolved_question: bool,
    hook_needs_input_sticky: bool,
    latest_usable_hook_at_ms: Option<i64>,
    terminal_notified: bool,
    notifications: Vec<NotificationEdge>,
}

pub fn reduce(events: impl IntoIterator<Item = AgentEvent>) -> Reduction {
    reduce_inner(events, None)
}

pub fn reduce_at(events: impl IntoIterator<Item = AgentEvent>, observed_now_ms: i64) -> Reduction {
    reduce_inner(events, Some(observed_now_ms))
}

fn reduce_inner(
    events: impl IntoIterator<Item = AgentEvent>,
    observed_now_ms: Option<i64>,
) -> Reduction {
    let mut seen = HashSet::new();
    let mut events: Vec<_> = events
        .into_iter()
        .filter(|event| seen.insert(event.dedupe_key.clone()))
        .collect();
    events.sort_by(AgentEvent::replay_cmp);

    let mut state = ReducerState::default();
    for event in events {
        apply(&mut state, event);
    }
    if let (Some(now), Some(projection)) = (observed_now_ms, state.projection.as_mut()) {
        if projection.source == EventSource::Transcript
            && projection.turn_state == TurnState::Running
            && now.saturating_sub(projection.last_observed_at_ms) > 30_000
        {
            projection.turn_state = TurnState::Waiting;
            projection.reason = StateReason::TranscriptIdle;
            projection.changed_at_ms = projection.last_observed_at_ms.saturating_add(30_001);
        }
    }
    Reduction {
        projection: state.projection,
        notifications: state.notifications,
    }
}

fn apply(state: &mut ReducerState, event: AgentEvent) {
    let observed_at = event.logical_at_ms();
    let existing = state.projection.clone();

    if event.source == EventSource::Transcript
        && (state.hook_needs_input_sticky
            || state
                .latest_usable_hook_at_ms
                .is_some_and(|hook_at| observed_at >= hook_at && observed_at - hook_at <= 120_000))
    {
        if let Some(projection) = state.projection.as_mut() {
            projection.revision += 1;
            projection.last_observed_at_ms = observed_at;
        }
        return;
    }

    if existing
        .as_ref()
        .is_some_and(|p| p.lifecycle == SessionLifecycle::Ended)
        && event.source_event != "SessionStart"
    {
        if let Some(projection) = state.projection.as_mut() {
            projection.revision += 1;
            projection.last_observed_at_ms = observed_at;
        }
        return;
    }

    let mut lifecycle = existing
        .as_ref()
        .map(|p| p.lifecycle)
        .unwrap_or(SessionLifecycle::Active);
    let mut turn_state = existing
        .as_ref()
        .map(|p| p.turn_state)
        .unwrap_or(TurnState::Waiting);
    let mut reason = existing
        .as_ref()
        .map(|p| p.reason)
        .unwrap_or(StateReason::Unknown);
    let before = (lifecycle, turn_state, reason);

    match event.source_event.as_str() {
        "SessionStart" => {
            lifecycle = SessionLifecycle::Active;
            turn_state = TurnState::Waiting;
            reason = StateReason::SessionStarted;
            state.unresolved_question = false;
            state.hook_needs_input_sticky = false;
            state.terminal_notified = false;
        }
        "UserPromptSubmit" => {
            lifecycle = SessionLifecycle::Active;
            turn_state = TurnState::Running;
            reason = StateReason::UserPromptSubmitted;
            state.unresolved_question = false;
            state.hook_needs_input_sticky = false;
            state.terminal_notified = false;
        }
        "PreToolUse" => {
            lifecycle = SessionLifecycle::Active;
            if payload_str(&event, "tool_name") == Some("AskUserQuestion") {
                if turn_state != TurnState::NeedsInput {
                    push_notification(state, NotificationKind::NeedsInput, &event);
                }
                turn_state = TurnState::NeedsInput;
                reason = StateReason::AskUserQuestion;
                state.unresolved_question = true;
                state.hook_needs_input_sticky = event.source == EventSource::Hook;
            } else {
                turn_state = TurnState::Running;
                reason = StateReason::ToolRunning;
                if event.source == EventSource::Hook {
                    state.hook_needs_input_sticky = false;
                    state.unresolved_question = false;
                }
            }
        }
        "PostToolUse" => {
            lifecycle = SessionLifecycle::Active;
            turn_state = TurnState::Running;
            reason = StateReason::ToolRunning;
            if event.source == EventSource::Hook {
                state.hook_needs_input_sticky = false;
                state.unresolved_question = false;
            }
        }
        "Notification" => {
            lifecycle = SessionLifecycle::Active;
            match payload_str(&event, "notification_type").unwrap_or_default() {
                "idle_prompt" => {
                    if !state.hook_needs_input_sticky {
                        turn_state = TurnState::Waiting;
                        reason = StateReason::IdlePrompt;
                    }
                }
                "auth_success" => {
                    turn_state = TurnState::Running;
                    reason = StateReason::AuthSuccess;
                    state.unresolved_question = false;
                    state.hook_needs_input_sticky = false;
                }
                "elicitation_complete" => {
                    turn_state = TurnState::Running;
                    reason = StateReason::ElicitationComplete;
                    state.unresolved_question = false;
                    state.hook_needs_input_sticky = false;
                }
                "elicitation_response" => {
                    turn_state = TurnState::Running;
                    reason = StateReason::ElicitationResponse;
                    state.unresolved_question = false;
                    state.hook_needs_input_sticky = false;
                }
                kind => {
                    if turn_state != TurnState::NeedsInput {
                        push_notification(state, NotificationKind::NeedsInput, &event);
                    }
                    turn_state = TurnState::NeedsInput;
                    reason = match kind {
                        "permission_prompt" => StateReason::PermissionPrompt,
                        "elicitation_dialog" => StateReason::ElicitationDialog,
                        _ => StateReason::UnknownNotification,
                    };
                    state.hook_needs_input_sticky = event.source == EventSource::Hook;
                }
            }
        }
        "Stop" => {
            if state.unresolved_question || state.hook_needs_input_sticky {
                turn_state = TurnState::NeedsInput;
            } else {
                turn_state = TurnState::Waiting;
                reason = StateReason::TurnStopped;
                if !state.terminal_notified {
                    push_notification(state, NotificationKind::Done, &event);
                    state.terminal_notified = true;
                }
            }
        }
        "StopFailure" => {
            turn_state = TurnState::Failed;
            reason = StateReason::StopFailed;
            state.unresolved_question = false;
            state.hook_needs_input_sticky = false;
            if !state.terminal_notified {
                push_notification(state, NotificationKind::Failed, &event);
                state.terminal_notified = true;
            }
        }
        "SessionEnd" => {
            lifecycle = SessionLifecycle::Ended;
            reason = StateReason::SessionEnded;
            state.hook_needs_input_sticky = false;
        }
        "TranscriptAssistantToolUse" => {
            lifecycle = SessionLifecycle::Active;
            turn_state = TurnState::Running;
            reason = StateReason::TranscriptToolUse;
        }
        "TranscriptAssistantThinking" => {
            lifecycle = SessionLifecycle::Active;
            turn_state = TurnState::Running;
            reason = StateReason::TranscriptThinking;
        }
        "TranscriptToolResult" => {
            lifecycle = SessionLifecycle::Active;
            turn_state = TurnState::Running;
            reason = StateReason::TranscriptToolResult;
        }
        "TranscriptAssistantText" => {
            lifecycle = SessionLifecycle::Active;
            let idle_ms = event
                .payload
                .get("idle_ms")
                .and_then(|value| value.as_i64())
                .unwrap_or_default();
            if idle_ms > 30_000 {
                turn_state = TurnState::Waiting;
                reason = StateReason::TranscriptIdle;
            } else {
                // Negative clock skew and the complete 8–30 second gray zone
                // are intentionally conservative.
                turn_state = TurnState::Running;
                reason = StateReason::TranscriptActive;
            }
        }
        _ => return,
    }

    let revision = state.projection.as_ref().map_or(1, |p| p.revision + 1);
    let changed_at_ms = if before != (lifecycle, turn_state, reason) {
        observed_at
    } else {
        existing.as_ref().map_or(observed_at, |p| p.changed_at_ms)
    };
    let confidence = if event.source == EventSource::Hook {
        Confidence::Definitive
    } else {
        Confidence::Inferred
    };
    if event.source == EventSource::Hook {
        state.latest_usable_hook_at_ms = Some(observed_at);
    }
    state.projection = Some(SessionProjection {
        agent_kind: event.agent_kind,
        session_id: event.session_id,
        lifecycle,
        turn_state,
        reason,
        source: event.source,
        confidence,
        revision,
        changed_at_ms,
        last_observed_at_ms: observed_at,
        current_turn_id: None,
    });

    let event_revision = revision;
    for notification in state
        .notifications
        .iter_mut()
        .rev()
        .take_while(|n| n.projection_revision == 0)
    {
        notification.projection_revision = event_revision;
    }
}

fn payload_str<'a>(event: &'a AgentEvent, key: &str) -> Option<&'a str> {
    event.payload.get(key).and_then(|value| value.as_str())
}

fn push_notification(state: &mut ReducerState, kind: NotificationKind, event: &AgentEvent) {
    state.notifications.push(NotificationEdge {
        kind,
        projection_revision: 0,
        triggering_event_id: event.id.clone(),
    });
}
