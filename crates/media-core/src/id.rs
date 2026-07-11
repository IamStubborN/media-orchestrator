macro_rules! define_id {
    ($name:ident) => {
        #[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
        pub struct $name(uuid::Uuid);

        impl $name {
            #[must_use]
            pub fn new() -> Self {
                Self(uuid::Uuid::new_v4())
            }

            #[must_use]
            pub const fn from_uuid(value: uuid::Uuid) -> Self {
                Self(value)
            }

            #[must_use]
            pub const fn as_uuid(&self) -> &uuid::Uuid {
                &self.0
            }

            #[must_use]
            pub const fn into_uuid(self) -> uuid::Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(formatter, "{}", self.0)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                uuid::Uuid::parse_str(value).map(Self)
            }
        }
    };
}

define_id!(UserId);
define_id!(ClientId);
define_id!(MediaId);
define_id!(SeasonId);
define_id!(EpisodeId);
define_id!(JobId);
define_id!(LeaseId);
define_id!(TaskId);
define_id!(JobEventId);

pub const PRIMARY_USER_ID: UserId = UserId::from_uuid(uuid::Uuid::from_u128(1));
pub const SECONDARY_USER_ID: UserId = UserId::from_uuid(uuid::Uuid::from_u128(2));
pub const PRIMARY_CLIENT_ID: ClientId =
    ClientId::from_uuid(uuid::Uuid::from_u128(0x00000000000000000001000000000001));
pub const SECONDARY_CLIENT_ID: ClientId =
    ClientId::from_uuid(uuid::Uuid::from_u128(0x00000000000000000001000000000002));
pub const RUNNER_CLIENT_ID: ClientId =
    ClientId::from_uuid(uuid::Uuid::from_u128(0x00000000000000000002000000000001));

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{
        PRIMARY_CLIENT_ID, PRIMARY_USER_ID, MediaId, RUNNER_CLIENT_ID, SECONDARY_CLIENT_ID,
        SECONDARY_USER_ID,
    };

    #[test]
    fn media_id_round_trips_through_text() {
        let id = MediaId::new();
        assert_eq!(MediaId::from_str(&id.to_string()).unwrap(), id);
    }

    #[test]
    fn media_id_converts_to_and_from_uuid_without_changing_value() {
        let raw = uuid::Uuid::new_v4();
        let id = MediaId::from_uuid(raw);

        assert_eq!(id.as_uuid(), &raw);
        assert_eq!(id.into_uuid(), raw);
    }

    #[test]
    fn fixed_identity_ids_match_the_bootstrap_contract() {
        assert_eq!(
            PRIMARY_USER_ID.to_string(),
            "00000000-0000-0000-0000-000000000001",
        );
        assert_eq!(
            SECONDARY_USER_ID.to_string(),
            "00000000-0000-0000-0000-000000000002",
        );
        assert_eq!(
            PRIMARY_CLIENT_ID.to_string(),
            "00000000-0000-0000-0001-000000000001",
        );
        assert_eq!(
            SECONDARY_CLIENT_ID.to_string(),
            "00000000-0000-0000-0001-000000000002",
        );
        assert_eq!(
            RUNNER_CLIENT_ID.to_string(),
            "00000000-0000-0000-0002-000000000001",
        );
    }
}
