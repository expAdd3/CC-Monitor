use async_trait::async_trait;
use monitor_domain::{AgentEvent, AgentKind, SessionId, SessionProjection};

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("storage operation failed: {0}")]
    Storage(String),
}

pub type RepositoryResult<T> = Result<T, RepositoryError>;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ProjectionKey {
    pub agent_kind: AgentKind,
    pub session_id: SessionId,
}

impl From<&SessionProjection> for ProjectionKey {
    fn from(projection: &SessionProjection) -> Self {
        Self {
            agent_kind: projection.agent_kind.clone(),
            session_id: projection.session_id.clone(),
        }
    }
}

#[async_trait]
pub trait EventRepository: Send + Sync {
    async fn append(&self, event: &AgentEvent) -> RepositoryResult<bool>;
    async fn for_session(
        &self,
        agent: AgentKind,
        session_id: &SessionId,
    ) -> RepositoryResult<Vec<AgentEvent>>;
}

#[async_trait]
pub trait ProjectionRepository: Send + Sync {
    async fn get(
        &self,
        agent: AgentKind,
        session_id: &SessionId,
    ) -> RepositoryResult<Option<SessionProjection>>;
    async fn put(&self, projection: &SessionProjection) -> RepositoryResult<()>;
}
