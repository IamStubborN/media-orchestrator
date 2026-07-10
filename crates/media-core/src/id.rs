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
define_id!(MediaId);
define_id!(SeasonId);
define_id!(EpisodeId);
define_id!(JobId);
define_id!(TaskId);

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::MediaId;

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
}
