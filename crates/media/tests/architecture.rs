const CORE_FORBIDDEN: &[&str] = &["axum", "reqwest", "sea-orm", "serde", "serde_json", "tokio"];

fn workspace_metadata() -> cargo_metadata::Metadata {
    cargo_metadata::MetadataCommand::new()
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .features(cargo_metadata::CargoOpt::AllFeatures)
        .exec()
        .unwrap()
}

fn workspace_package_id<'a>(
    metadata: &'a cargo_metadata::Metadata,
    name: &str,
) -> &'a cargo_metadata::PackageId {
    metadata
        .workspace_members
        .iter()
        .find(|package_id| metadata[*package_id].name.as_str() == name)
        .unwrap_or_else(|| panic!("workspace package {name} was not found"))
}

fn direct_dependency_package_ids<'a>(
    metadata: &'a cargo_metadata::Metadata,
    package_id: &cargo_metadata::PackageId,
) -> Vec<&'a cargo_metadata::PackageId> {
    metadata
        .resolve
        .as_ref()
        .expect("Cargo metadata did not include a resolved dependency graph")
        .nodes
        .iter()
        .find(|node| node.id == *package_id)
        .unwrap_or_else(|| panic!("resolved package {package_id} was not found"))
        .deps
        .iter()
        .map(|dependency| &dependency.pkg)
        .collect()
}

#[test]
fn package_id_helpers_select_workspace_members_and_resolved_edges() {
    let metadata = workspace_metadata();
    let media = workspace_package_id(&metadata, "media");
    let core = workspace_package_id(&metadata, "media-core");
    let contract = workspace_package_id(&metadata, "media-contract");
    let direct_dependencies = direct_dependency_package_ids(&metadata, media);

    assert!(metadata.workspace_members.contains(media));
    assert!(direct_dependencies.contains(&core));
    assert!(direct_dependencies.contains(&contract));
}

#[test]
fn media_core_has_no_forbidden_direct_dependencies() {
    let metadata = workspace_metadata();
    let core = workspace_package_id(&metadata, "media-core");
    let actual: Vec<&str> = direct_dependency_package_ids(&metadata, core)
        .into_iter()
        .map(|package_id| metadata[package_id].name.as_str())
        .collect();

    for forbidden in CORE_FORBIDDEN {
        assert!(
            !actual.contains(forbidden),
            "media-core depends on {forbidden}",
        );
    }

    assert!(
        direct_dependency_package_ids(&metadata, core)
            .iter()
            .all(|package_id| !metadata.workspace_members.contains(package_id)),
        "media-core must not depend on another workspace crate",
    );
}

#[test]
fn media_contract_does_not_depend_on_media_core() {
    let metadata = workspace_metadata();
    let contract = workspace_package_id(&metadata, "media-contract");
    let core = workspace_package_id(&metadata, "media-core");

    assert!(
        !direct_dependency_package_ids(&metadata, contract).contains(&core),
        "media-contract must not depend on media-core",
    );
}
