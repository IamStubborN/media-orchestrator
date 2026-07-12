use url::Url;

use crate::RezkaError;

#[derive(Clone)]
pub struct MirrorSet {
    origins: Vec<Url>,
    selected: usize,
}

impl std::fmt::Debug for MirrorSet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "MirrorSet {{ origins: [REDACTED], selected: {} }}",
            self.selected
        )
    }
}

impl MirrorSet {
    pub fn new(origins: Vec<Url>) -> Result<Self, RezkaError> {
        if origins.is_empty() || origins.iter().any(|origin| !is_valid_origin(origin)) {
            return Err(invalid_mirror_origin());
        }

        Ok(Self {
            origins,
            selected: 0,
        })
    }

    #[must_use]
    pub fn primary_origin(&self) -> &Url {
        &self.origins[0]
    }

    #[must_use]
    pub fn selected_origin(&self) -> &Url {
        &self.origins[self.selected]
    }

    #[must_use]
    pub fn contains_origin(&self, candidate: &Url) -> bool {
        self.origins
            .iter()
            .any(|origin| same_origin(origin, candidate))
    }

    pub fn rewrite_to_selected(&self, url: &Url) -> Result<Url, RezkaError> {
        let mut rewritten = self.selected_origin().clone();
        rewritten.set_path(url.path());
        rewritten.set_query(url.query());
        rewritten.set_fragment(None);
        Ok(rewritten)
    }

    pub fn select_next(&mut self) -> bool {
        if self.selected + 1 >= self.origins.len() {
            return false;
        }

        self.selected += 1;
        true
    }

    pub(crate) fn select_matching_origin(&mut self, predicate: impl Fn(&Url) -> bool) -> bool {
        let Some(selected) = self.origins.iter().position(predicate) else {
            return false;
        };
        self.origins.rotate_left(selected);
        self.selected = 0;
        true
    }

    pub(crate) fn promote_selected(&mut self) {
        self.origins.rotate_left(self.selected);
        self.selected = 0;
    }

    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.origins.len()
    }
}

#[must_use]
pub(crate) fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host() == right.host()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn is_valid_origin(origin: &Url) -> bool {
    matches!(origin.scheme(), "http" | "https")
        && origin.host().is_some()
        && origin.port_or_known_default().is_some()
        && origin.username().is_empty()
        && origin.password().is_none()
        && origin.path() == "/"
        && origin.query().is_none()
        && origin.fragment().is_none()
}

fn invalid_mirror_origin() -> RezkaError {
    RezkaError::Configuration {
        message: "invalid mirror origin",
    }
}
