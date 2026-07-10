const CORE_FORBIDDEN: &[&str] = &["axum", "reqwest", "sea-orm", "serde", "serde_json", "tokio"];

fn workspace_metadata() -> cargo_metadata::Metadata {
    cargo_metadata::MetadataCommand::new()
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .exec()
        .unwrap()
}

#[test]
fn media_core_has_no_forbidden_direct_dependencies() {
    let metadata = workspace_metadata();
    let core = metadata
        .packages
        .iter()
        .find(|package| package.name.as_str() == "media-core")
        .unwrap();
    let actual: Vec<&str> = core
        .dependencies
        .iter()
        .map(|dependency| dependency.name.as_str())
        .collect();

    for forbidden in CORE_FORBIDDEN {
        assert!(
            !actual.contains(forbidden),
            "media-core depends on {forbidden}",
        );
    }

    let workspace_names: std::collections::HashSet<&str> = metadata
        .workspace_packages()
        .iter()
        .map(|package| package.name.as_str())
        .collect();
    assert!(
        core.dependencies
            .iter()
            .all(|dependency| !workspace_names.contains(dependency.name.as_str())),
        "media-core must not depend on another workspace crate",
    );
}

#[test]
fn media_contract_does_not_depend_on_media_core() {
    let metadata = workspace_metadata();
    let contract = metadata
        .packages
        .iter()
        .find(|package| package.name.as_str() == "media-contract")
        .unwrap();

    assert!(
        contract
            .dependencies
            .iter()
            .all(|dependency| dependency.name.as_str() != "media-core"),
    );
}
