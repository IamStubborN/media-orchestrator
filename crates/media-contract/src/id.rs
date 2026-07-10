#[derive(
    Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct PublicId(uuid::Uuid);

impl PublicId {
    pub fn parse(value: &str) -> Result<Self, uuid::Error> {
        uuid::Uuid::parse_str(value).map(Self)
    }

    #[must_use]
    pub const fn as_uuid(&self) -> &uuid::Uuid {
        &self.0
    }
}

impl std::fmt::Display for PublicId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::PublicId;

    #[test]
    fn invalid_public_id_is_rejected() {
        assert!(PublicId::parse("not-a-uuid").is_err());
    }

    #[test]
    fn public_id_round_trips_as_a_json_string() {
        let text = "018f3f86-7b4c-7b4f-9b6a-6d62f45bb111";
        let id = PublicId::parse(text).unwrap();
        let encoded = serde_json::to_string(&id).unwrap();

        assert_eq!(encoded, format!("\"{text}\""));
        assert_eq!(serde_json::from_str::<PublicId>(&encoded).unwrap(), id);
        assert_eq!(id.as_uuid().to_string(), text);
    }
}
