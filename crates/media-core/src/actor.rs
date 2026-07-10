use crate::{
    PRIMARY_CLIENT_ID, PRIMARY_USER_ID, ClientId, RUNNER_CLIENT_ID, UserId, SECONDARY_CLIENT_ID,
    SECONDARY_USER_ID,
};

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum ClientRole {
    Hermes,
    Runner,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Actor {
    client_id: ClientId,
    user_id: Option<UserId>,
    role: ClientRole,
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

    #[must_use]
    pub const fn client_id(&self) -> ClientId {
        self.client_id
    }

    #[must_use]
    pub const fn user_id(&self) -> Option<UserId> {
        self.user_id
    }

    #[must_use]
    pub const fn role(&self) -> ClientRole {
        self.role
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
    client_id: ClientId,
    name: String,
    role: ClientRole,
    user_id: Option<UserId>,
    digest: CredentialDigest,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum BootstrapClientError {
    #[error("client name cannot be empty")]
    EmptyName,
    #[error("invalid client identity: {0}")]
    InvalidIdentity(#[source] ActorError),
    #[error("client identity is not part of the fixed bootstrap set")]
    UnsupportedFixedIdentity,
}

impl BootstrapClient {
    pub fn new(
        client_id: ClientId,
        name: String,
        role: ClientRole,
        user_id: Option<UserId>,
        digest: CredentialDigest,
    ) -> Result<Self, BootstrapClientError> {
        let actor =
            Actor::new(client_id, user_id, role).map_err(BootstrapClientError::InvalidIdentity)?;
        if !is_fixed_bootstrap_identity(&actor) {
            return Err(BootstrapClientError::UnsupportedFixedIdentity);
        }
        let name = name.trim().to_owned();
        if name.is_empty() {
            return Err(BootstrapClientError::EmptyName);
        }

        Ok(Self {
            client_id: actor.client_id(),
            name,
            role: actor.role(),
            user_id: actor.user_id(),
            digest,
        })
    }

    #[must_use]
    pub const fn client_id(&self) -> ClientId {
        self.client_id
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn role(&self) -> ClientRole {
        self.role
    }

    #[must_use]
    pub const fn user_id(&self) -> Option<UserId> {
        self.user_id
    }

    #[must_use]
    pub const fn digest(&self) -> &CredentialDigest {
        &self.digest
    }
}

fn is_fixed_bootstrap_identity(actor: &Actor) -> bool {
    matches!(
        (actor.client_id(), actor.role(), actor.user_id()),
        (PRIMARY_CLIENT_ID, ClientRole::Hermes, Some(PRIMARY_USER_ID))
            | (
                SECONDARY_CLIENT_ID,
                ClientRole::Hermes,
                Some(SECONDARY_USER_ID)
            )
            | (RUNNER_CLIENT_ID, ClientRole::Runner, None)
    )
}

#[cfg(test)]
mod tests {
    use super::{
        Actor, ActorError, BootstrapClient, BootstrapClientError, ClientRole, CredentialDigest,
    };
    use crate::{PRIMARY_CLIENT_ID, PRIMARY_USER_ID, ClientId, RUNNER_CLIENT_ID, SECONDARY_USER_ID};

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
    fn actor_exposes_validated_identity_through_read_only_accessors() {
        let actor = Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap();

        assert_eq!(actor.client_id(), PRIMARY_CLIENT_ID);
        assert_eq!(actor.user_id(), Some(PRIMARY_USER_ID));
        assert_eq!(actor.role(), ClientRole::Hermes);
    }

    #[test]
    fn credential_digest_debug_output_is_redacted() {
        let digest = CredentialDigest::from([0x2a; 32]);

        assert_eq!(digest.as_bytes(), &[0x2a; 32]);
        assert_eq!(format!("{digest:?}"), "CredentialDigest([REDACTED])");
        assert!(!format!("{digest:?}").contains("42"));
    }

    #[test]
    fn bootstrap_client_rejects_invalid_role_user_mappings() {
        let digest = CredentialDigest::from([0x2a; 32]);

        assert_eq!(
            BootstrapClient::new(
                PRIMARY_CLIENT_ID,
                "primary".to_owned(),
                ClientRole::Hermes,
                None,
                digest,
            )
            .unwrap_err(),
            BootstrapClientError::InvalidIdentity(ActorError::HermesUserRequired),
        );
        assert_eq!(
            BootstrapClient::new(
                RUNNER_CLIENT_ID,
                "runner".to_owned(),
                ClientRole::Runner,
                Some(PRIMARY_USER_ID),
                digest,
            )
            .unwrap_err(),
            BootstrapClientError::InvalidIdentity(ActorError::RunnerCannotHaveUser),
        );
    }

    #[test]
    fn bootstrap_client_rejects_swapped_and_unknown_fixed_identities() {
        let digest = CredentialDigest::from([0x2a; 32]);

        assert_eq!(
            BootstrapClient::new(
                PRIMARY_CLIENT_ID,
                "swapped".to_owned(),
                ClientRole::Hermes,
                Some(SECONDARY_USER_ID),
                digest,
            )
            .unwrap_err(),
            BootstrapClientError::UnsupportedFixedIdentity,
        );
        assert_eq!(
            BootstrapClient::new(
                ClientId::new(),
                "unknown".to_owned(),
                ClientRole::Runner,
                None,
                digest,
            )
            .unwrap_err(),
            BootstrapClientError::UnsupportedFixedIdentity,
        );
    }

    #[test]
    fn bootstrap_client_rejects_a_blank_name() {
        let error = BootstrapClient::new(
            PRIMARY_CLIENT_ID,
            " \t\n".to_owned(),
            ClientRole::Hermes,
            Some(PRIMARY_USER_ID),
            CredentialDigest::from([0x2a; 32]),
        )
        .unwrap_err();

        assert_eq!(error, BootstrapClientError::EmptyName);
    }

    #[test]
    fn bootstrap_client_exposes_only_validated_read_only_values() {
        let digest = CredentialDigest::from([0x2a; 32]);
        let client = BootstrapClient::new(
            PRIMARY_CLIENT_ID,
            "  primary  ".to_owned(),
            ClientRole::Hermes,
            Some(PRIMARY_USER_ID),
            digest,
        )
        .unwrap();

        assert_eq!(client.client_id(), PRIMARY_CLIENT_ID);
        assert_eq!(client.name(), "primary");
        assert_eq!(client.role(), ClientRole::Hermes);
        assert_eq!(client.user_id(), Some(PRIMARY_USER_ID));
        assert_eq!(client.digest(), &digest);
    }
}
