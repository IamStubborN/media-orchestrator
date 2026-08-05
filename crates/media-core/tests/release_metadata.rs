use media_core::{ReleaseCandidate, ReleaseLifecycle, ReleaseQuery, select_release_candidate};

fn candidate(
    id: u64,
    title: &str,
    original_title: Option<&str>,
    year: Option<i32>,
) -> ReleaseCandidate {
    ReleaseCandidate {
        source_id: id,
        title: title.to_owned(),
        original_title: original_title.map(str::to_owned),
        year,
        lifecycle: ReleaseLifecycle::Unknown,
    }
}

#[test]
fn exact_original_title_and_year_selects_one_candidate() {
    let query = ReleaseQuery::new("Разделение", Some("Severance".to_owned()), Some(2022)).unwrap();
    let candidates = vec![
        candidate(1, "Severance", None, Some(2022)),
        candidate(2, "Severance", None, Some(2024)),
    ];

    assert_eq!(select_release_candidate(&query, &candidates), Some(0));
}

#[test]
fn duplicate_exact_matches_remain_ambiguous() {
    let query = ReleaseQuery::new("The Office", None, Some(2005)).unwrap();
    let candidates = vec![
        candidate(1, "The Office", None, Some(2005)),
        candidate(2, "The Office", Some("The Office"), Some(2005)),
    ];

    assert_eq!(select_release_candidate(&query, &candidates), None);
}

#[test]
fn title_only_does_not_auto_select_from_multiple_candidates() {
    let query = ReleaseQuery::new("The Office", None, None).unwrap();
    let candidates = vec![
        candidate(1, "The Office", None, Some(2005)),
        candidate(2, "The Office", None, Some(2024)),
    ];

    assert_eq!(select_release_candidate(&query, &candidates), None);
}

#[test]
fn title_only_selects_the_only_ongoing_exact_match() {
    let query = ReleaseQuery::new("Silo", None, None).unwrap();
    let candidates = vec![
        ReleaseCandidate {
            lifecycle: ReleaseLifecycle::Ongoing,
            ..candidate(1, "Silo", None, Some(2023))
        },
        ReleaseCandidate {
            lifecycle: ReleaseLifecycle::Ended,
            ..candidate(2, "Silo", None, Some(2017))
        },
    ];

    assert_eq!(select_release_candidate(&query, &candidates), Some(0));
}

#[test]
fn title_only_keeps_multiple_ongoing_exact_matches_ambiguous() {
    let query = ReleaseQuery::new("Sugar", None, None).unwrap();
    let candidates = vec![
        ReleaseCandidate {
            lifecycle: ReleaseLifecycle::Ongoing,
            ..candidate(1, "Sugar", None, Some(2024))
        },
        ReleaseCandidate {
            lifecycle: ReleaseLifecycle::Ongoing,
            ..candidate(2, "Sugar", None, Some(2018))
        },
    ];

    assert_eq!(select_release_candidate(&query, &candidates), None);
}

#[test]
fn sole_fuzzy_candidate_is_not_auto_selected() {
    let query = ReleaseQuery::new("Office", None, Some(2005)).unwrap();
    let candidates = vec![candidate(1, "The Office", None, Some(2005))];

    assert_eq!(select_release_candidate(&query, &candidates), None);
}

#[test]
fn source_id_builder_is_optional_and_rejects_zero() {
    let query = ReleaseQuery::new("Lucky", None, None).unwrap();
    assert_eq!(query.source_id, None);
    assert!(query.clone().with_source_id(0).is_err());
    assert_eq!(query.with_source_id(77).unwrap().source_id, Some(77));
}
