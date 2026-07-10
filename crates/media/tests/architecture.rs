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

fn resolved_dependency_reachable(
    metadata: &cargo_metadata::Metadata,
    source: &cargo_metadata::PackageId,
    target: &cargo_metadata::PackageId,
) -> bool {
    let nodes = &metadata
        .resolve
        .as_ref()
        .expect("Cargo metadata did not include a resolved dependency graph")
        .nodes;
    let mut pending = vec![source];
    let mut visited = std::collections::HashSet::new();

    while let Some(package_id) = pending.pop() {
        if !visited.insert(package_id) {
            continue;
        }

        let node = nodes
            .iter()
            .find(|node| node.id == *package_id)
            .unwrap_or_else(|| panic!("resolved package {package_id} was not found"));

        for dependency in &node.deps {
            if dependency.pkg == *target {
                return true;
            }
            pending.push(&dependency.pkg);
        }
    }

    false
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
fn resolved_graph_reachability_follows_transitive_edges() {
    let metadata = workspace_metadata();
    let media = workspace_package_id(&metadata, "media");
    let serde = &metadata
        .packages
        .iter()
        .find(|package| package.name.as_str() == "serde")
        .expect("resolved package serde was not found")
        .id;

    assert!(!direct_dependency_package_ids(&metadata, media).contains(&serde));
    assert!(resolved_dependency_reachable(&metadata, media, serde));
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
fn media_contract_cannot_reach_media_core() {
    let metadata = workspace_metadata();
    let contract = workspace_package_id(&metadata, "media-contract");
    let core = workspace_package_id(&metadata, "media-core");

    assert!(
        !resolved_dependency_reachable(&metadata, contract, core),
        "media-contract must not reach media-core through the resolved dependency graph",
    );
}
