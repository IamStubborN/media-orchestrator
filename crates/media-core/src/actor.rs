use crate::{ClientId, UserId};

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum ClientRole {
    Hermes,
    Runner,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Actor {
    pub client_id: ClientId,
    pub user_id: Option<UserId>,
    pub role: ClientRole,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum ActorError {
    #[error("Hermes clients require a user identity")]
    HermesUserRequired,
    #[error("runner clients cannot have a user identity")]
    RunnerCannotHaveUser,
    #[error("runner clients cannot access user operations")]
    UserAccessForbidden,
    #[error("this operation requires a runner client")]
    RunnerAccessRequired,
}

impl Actor {
    pub fn new(
        client_id: ClientId,
        user_id: Option<UserId>,
        role: ClientRole,
    ) -> Result<Self, ActorError> {
        match (role, user_id) {
            (ClientRole::Hermes, None) => Err(ActorError::HermesUserRequired),
            (ClientRole::Runner, Some(_)) => Err(ActorError::RunnerCannotHaveUser),
            _ => Ok(Self {
                client_id,
                user_id,
                role,
            }),
        }
    }

    pub fn require_user(&self) -> Result<UserId, ActorError> {
        match (self.role, self.user_id) {
            (ClientRole::Hermes, Some(user_id)) => Ok(user_id),
            (ClientRole::Hermes, None) => Err(ActorError::HermesUserRequired),
            (ClientRole::Runner, _) => Err(ActorError::UserAccessForbidden),
        }
    }

    pub fn require_runner(&self) -> Result<ClientId, ActorError> {
        match (self.role, self.user_id) {
            (ClientRole::Runner, None) => Ok(self.client_id),
            (ClientRole::Runner, Some(_)) => Err(ActorError::RunnerCannotHaveUser),
            (ClientRole::Hermes, _) => Err(ActorError::RunnerAccessRequired),
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub struct CredentialDigest([u8; 32]);

impl CredentialDigest {
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl From<[u8; 32]> for CredentialDigest {
    fn from(value: [u8; 32]) -> Self {
        Self(value)
    }
}

impl std::fmt::Debug for CredentialDigest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CredentialDigest([REDACTED])")
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BootstrapClient {
    pub client_id: ClientId,
    pub name: String,
    pub role: ClientRole,
    pub user_id: Option<UserId>,
    pub digest: CredentialDigest,
}

#[cfg(test)]
mod tests {
    use super::{Actor, ActorError, ClientRole, CredentialDigest};
    use crate::{PRIMARY_CLIENT_ID, PRIMARY_USER_ID, RUNNER_CLIENT_ID};

    #[test]
    fn hermes_actor_requires_a_user() {
        let error = Actor::new(PRIMARY_CLIENT_ID, None, ClientRole::Hermes).unwrap_err();

        assert_eq!(error, ActorError::HermesUserRequired);
    }

    #[test]
    fn runner_actor_rejects_a_user_identity() {
        let error =
            Actor::new(RUNNER_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Runner).unwrap_err();

        assert_eq!(error, ActorError::RunnerCannotHaveUser);
    }

    #[test]
    fn runner_actor_cannot_access_user_operations() {
        let actor = Actor::new(RUNNER_CLIENT_ID, None, ClientRole::Runner).unwrap();

        assert_eq!(actor.require_user(), Err(ActorError::UserAccessForbidden));
    }

    #[test]
    fn hermes_actor_cannot_access_runner_operations() {
        let actor = Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap();

        assert_eq!(
            actor.require_runner(),
            Err(ActorError::RunnerAccessRequired)
        );
    }

    #[test]
    fn credential_digest_debug_output_is_redacted() {
        let digest = CredentialDigest::from([0x2a; 32]);

        assert_eq!(digest.as_bytes(), &[0x2a; 32]);
        assert_eq!(format!("{digest:?}"), "CredentialDigest([REDACTED])");
        assert!(!format!("{digest:?}").contains("42"));
    }
}
