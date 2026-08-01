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

#[test]
fn media_client_depends_only_on_the_wire_contract() {
    let metadata = workspace_metadata();
    let client = workspace_package_id(&metadata, "media-client");
    let mut workspace_dependencies: Vec<&str> = direct_dependency_package_ids(&metadata, client)
        .into_iter()
        .filter(|package_id| metadata.workspace_members.contains(package_id))
        .map(|package_id| metadata[package_id].name.as_str())
        .collect();
    workspace_dependencies.sort_unstable();

    assert_eq!(workspace_dependencies, ["media-contract"]);
    for forbidden_name in ["media-api", "media-core", "media-storage"] {
        let forbidden = workspace_package_id(&metadata, forbidden_name);
        assert!(
            !resolved_dependency_reachable(&metadata, client, forbidden),
            "media-client must not reach {forbidden_name}",
        );
    }
}

#[test]
fn media_api_depends_only_on_core_and_contract_workspace_crates() {
    let metadata = workspace_metadata();
    let api = workspace_package_id(&metadata, "media-api");
    let mut workspace_dependencies: Vec<&str> = direct_dependency_package_ids(&metadata, api)
        .into_iter()
        .filter(|package_id| metadata.workspace_members.contains(package_id))
        .map(|package_id| metadata[package_id].name.as_str())
        .collect();
    workspace_dependencies.sort_unstable();

    assert_eq!(workspace_dependencies, ["media-contract", "media-core"]);
}

#[test]
fn media_api_cannot_reach_storage() {
    let metadata = workspace_metadata();
    let api = workspace_package_id(&metadata, "media-api");
    let storage = workspace_package_id(&metadata, "media-storage");

    assert!(
        !resolved_dependency_reachable(&metadata, api, storage),
        "media-api must not reach media-storage through the resolved dependency graph",
    );
}

#[test]
fn media_storage_cannot_reach_api_or_contract() {
    let metadata = workspace_metadata();
    let storage = workspace_package_id(&metadata, "media-storage");

    for forbidden_name in ["media-api", "media-contract"] {
        let forbidden = workspace_package_id(&metadata, forbidden_name);
        assert!(
            !resolved_dependency_reachable(&metadata, storage, forbidden),
            "media-storage must not reach {forbidden_name} through the resolved dependency graph",
        );
    }
}

#[test]
fn rezka_client_has_no_workspace_dependencies() {
    let metadata = workspace_metadata();
    let rezka = workspace_package_id(&metadata, "rezka-client");
    let workspace_dependencies: Vec<&str> = direct_dependency_package_ids(&metadata, rezka)
        .into_iter()
        .filter(|package_id| metadata.workspace_members.contains(package_id))
        .map(|package_id| metadata[package_id].name.as_str())
        .collect();

    assert!(
        workspace_dependencies.is_empty(),
        "rezka-client must stay independent of media workspace crates: {workspace_dependencies:?}",
    );
}

#[test]
fn media_runner_has_no_storage_api_or_database_dependencies() {
    let metadata = workspace_metadata();
    let runner = workspace_package_id(&metadata, "media-runner");

    for forbidden_name in ["media-api", "media-storage", "sea-orm", "sea-orm-migration"] {
        let forbidden = metadata
            .packages
            .iter()
            .find(|package| package.name.as_str() == forbidden_name)
            .map(|package| &package.id);
        if let Some(forbidden) = forbidden {
            assert!(
                !resolved_dependency_reachable(&metadata, runner, forbidden),
                "media-runner must not reach {forbidden_name}",
            );
        }
    }
}

#[test]
fn media_core_and_contract_cannot_reach_rezka_client() {
    // Phase 4 boundary guard. The domain playback manifest and the ephemeral
    // secret CDN/subtitle URL wrappers deliberately implement no Serde (asserted
    // directly at the type level in `rezka-client`'s `live_probe` test, where
    // serde is a nameable dependency). This test enforces the complementary
    // crate-boundary invariant that cargo metadata can express honestly: the
    // serializable domain (`media-core`) and wire-contract (`media-contract`)
    // crates cannot reach `rezka-client` at all, so no provider type — Serde or
    // not — can ever be embedded in a serialized domain or wire payload. A naive
    // "rezka-client does not depend on serde" check would be false, because
    // rezka-client legitimately uses serde for its custom JSON deserializers and
    // for session-cookie persistence.
    let metadata = workspace_metadata();
    let rezka = workspace_package_id(&metadata, "rezka-client");

    for domain_crate in ["media-core", "media-contract"] {
        let source = workspace_package_id(&metadata, domain_crate);
        assert!(
            !resolved_dependency_reachable(&metadata, source, rezka),
            "{domain_crate} must not reach rezka-client through the resolved dependency graph",
        );
    }
}

#[test]
fn media_runner_depends_only_on_rezka_client_workspace_crate_in_phase_3() {
    let metadata = workspace_metadata();
    let runner = workspace_package_id(&metadata, "media-runner");
    let mut workspace_dependencies: Vec<&str> = direct_dependency_package_ids(&metadata, runner)
        .into_iter()
        .filter(|package_id| metadata.workspace_members.contains(package_id))
        .map(|package_id| metadata[package_id].name.as_str())
        .collect();
    workspace_dependencies.sort_unstable();

    assert_eq!(workspace_dependencies, ["rezka-client"]);
}
