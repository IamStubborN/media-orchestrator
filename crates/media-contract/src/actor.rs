#[derive(Debug, Copy, Clone, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderDto {
    Rezka,
    Prowlarr,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyScopeDto {
    Initiator,
    Family,
}

#[cfg(test)]
mod tests {
    use super::{NotifyScopeDto, ProviderDto};

    #[test]
    fn actor_enums_use_stable_snake_case_names() {
        let providers = [
            (ProviderDto::Rezka, "\"rezka\""),
            (ProviderDto::Prowlarr, "\"prowlarr\""),
        ];
        for (value, json) in providers {
            assert_eq!(serde_json::to_string(&value).unwrap(), json);
            assert_eq!(serde_json::from_str::<ProviderDto>(json).unwrap(), value);
        }

        let scopes = [
            (NotifyScopeDto::Initiator, "\"initiator\""),
            (NotifyScopeDto::Family, "\"family\""),
        ];
        for (value, json) in scopes {
            assert_eq!(serde_json::to_string(&value).unwrap(), json);
            assert_eq!(serde_json::from_str::<NotifyScopeDto>(json).unwrap(), value,);
        }
    }
}
