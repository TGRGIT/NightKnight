//! The authorization scope model — Nightscout v3 style `{api}:{collection}:{action}`.
//!
//! A **granted scope** is three colon-separated segments, each either a concrete
//! value or the wildcard `*`, e.g. `api:entries:read`, `api:*:read`, `*:*:*`.
//! A **required permission** is three concrete segments describing what an operation
//! needs. A scope *grants* a permission when each segment matches (equal, or the
//! scope's segment is `*`).
//!
//! Human users authenticated through the identity provider own their data and hold
//! the all-access scope ([`ScopeSet::all`]); machine device-tokens hold only the
//! narrow scopes they were issued. Read access is also implied by write access in
//! Nightscout, so a `create`/`update`/`delete`/`admin` grant satisfies a `read`
//! requirement on the same collection.

use std::fmt;

/// An action a request performs on a collection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Read,
    Create,
    Update,
    Delete,
    Admin,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Read => "read",
            Action::Create => "create",
            Action::Update => "update",
            Action::Delete => "delete",
            Action::Admin => "admin",
        }
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A concrete permission an operation requires, e.g. `api` + `entries` + `read`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Permission {
    pub api: String,
    pub collection: String,
    pub action: Action,
}

impl Permission {
    /// Build the common case: an `api` permission on a collection.
    pub fn api(collection: impl Into<String>, action: Action) -> Permission {
        Permission {
            api: "api".to_string(),
            collection: collection.into(),
            action,
        }
    }
}

/// A single granted scope (three segments, each concrete or `*`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    api: String,
    collection: String,
    action: String,
}

impl Scope {
    /// Parse `"api:entries:read"`. Returns `None` if it isn't exactly three non-empty
    /// segments.
    pub fn parse(s: &str) -> Option<Scope> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
            return None;
        }
        Some(Scope {
            api: parts[0].to_string(),
            collection: parts[1].to_string(),
            action: parts[2].to_string(),
        })
    }

    /// The all-access scope `*:*:*`.
    pub fn wildcard() -> Scope {
        Scope {
            api: "*".into(),
            collection: "*".into(),
            action: "*".into(),
        }
    }

    fn seg_matches(granted: &str, required: &str) -> bool {
        granted == "*" || granted == required
    }

    /// Whether this scope grants the given permission. Write actions imply read.
    pub fn grants(&self, perm: &Permission) -> bool {
        if !Self::seg_matches(&self.api, &perm.api) {
            return false;
        }
        if !Self::seg_matches(&self.collection, &perm.collection) {
            return false;
        }
        // Action match: exact, wildcard, or write-implies-read.
        if Self::seg_matches(&self.action, perm.action.as_str()) {
            return true;
        }
        perm.action == Action::Read
            && matches!(self.action.as_str(), "create" | "update" | "delete" | "admin")
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.api, self.collection, self.action)
    }
}

/// A set of granted scopes.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ScopeSet(Vec<Scope>);

impl ScopeSet {
    /// Parse a list of scope strings, silently dropping any that are malformed.
    pub fn parse_all<I, S>(items: I) -> ScopeSet
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        ScopeSet(items.into_iter().filter_map(|s| Scope::parse(s.as_ref())).collect())
    }

    /// The all-access set (`*:*:*`) granted to authenticated owners.
    pub fn all() -> ScopeSet {
        ScopeSet(vec![Scope::wildcard()])
    }

    /// Whether any scope in the set grants the permission.
    pub fn grants(&self, perm: &Permission) -> bool {
        self.0.iter().any(|s| s.grants(perm))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn scopes(&self) -> &[Scope] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A narrow grant lets through exactly what it names and nothing else — the
    /// foundation of least-privilege device tokens.
    #[test]
    fn exact_scope_grants_only_itself() {
        let set = ScopeSet::parse_all(["api:entries:create"]);
        assert!(set.grants(&Permission::api("entries", Action::Create)));
        assert!(!set.grants(&Permission::api("entries", Action::Delete)));
        assert!(!set.grants(&Permission::api("treatments", Action::Create)));
    }

    /// Collection wildcard lets a read-only token read every collection but write
    /// none — a typical "follower" token for a caregiver.
    #[test]
    fn collection_wildcard_read_token() {
        let set = ScopeSet::parse_all(["api:*:read"]);
        assert!(set.grants(&Permission::api("entries", Action::Read)));
        assert!(set.grants(&Permission::api("treatments", Action::Read)));
        assert!(!set.grants(&Permission::api("entries", Action::Create)));
    }

    /// Write access implies read access on the same collection (Nightscout semantics)
    /// — an uploader that can create entries can also read them back.
    #[test]
    fn write_implies_read() {
        let set = ScopeSet::parse_all(["api:entries:create"]);
        assert!(set.grants(&Permission::api("entries", Action::Read)));
        // …but not read on a different collection.
        assert!(!set.grants(&Permission::api("treatments", Action::Read)));
    }

    /// The owner's all-access set grants everything.
    #[test]
    fn all_access_grants_everything() {
        let set = ScopeSet::all();
        assert!(set.grants(&Permission::api("entries", Action::Delete)));
        assert!(set.grants(&Permission::api("settings", Action::Admin)));
    }

    /// Malformed scope strings are dropped, never silently widening access.
    #[test]
    fn malformed_scopes_are_ignored() {
        let set = ScopeSet::parse_all(["", "api:entries", "a:b:c:d", "api::read"]);
        assert!(set.is_empty(), "no malformed scope should be retained");
    }
}
