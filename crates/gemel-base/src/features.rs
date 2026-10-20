//! Feature registry used during connection negotiation.
//!
//! Ports Unison `src/features.ml`.
//!
//! Derived from Unison, Copyright 2021, Tõivo Leedjärv.
//!
//! Client and server each register the features they support. During the
//! connection handshake they agree on a set (`negotiate.ml`), validate it,
//! and enable it; codecs then test [`Features::enabled`] to decide what to
//! send. OCaml keeps the registry in a global table; here it is a value
//! owned by the session, and [`Features::register`] returns the handle that
//! modules test.

use std::collections::BTreeMap;

use crate::prefs::Prefs;
use crate::util::Error;

/// A feature's identifier: its name, as exchanged with the peer.
pub type Id = String;

/// Checks a negotiated feature set: called with the set, whether this
/// feature is in it, and the preferences; returns an error message to
/// reject the set.
pub type Validator = Box<dyn Fn(&[Id], bool, &Prefs) -> Option<String> + Send + Sync>;

/// A registered feature, to test with [`Features::enabled`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Feature(Option<usize>);

impl Feature {
    /// `Features.dummy`: never negotiated and never enabled.
    pub const DUMMY: Feature = Feature(None);
}

struct Entry {
    name: Id,
    enabled: bool,
    arc_format_change: bool,
    validator: Option<Validator>,
}

/// The set of features this side supports, and which are enabled.
#[derive(Default)]
pub struct Features {
    entries: Vec<Entry>,
}

impl std::fmt::Debug for Features {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.entries.iter().map(|e| (&e.name, e.enabled)))
            .finish()
    }
}

impl Features {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// `Features.register`. `arc_format_change` says whether an archive
    /// written with the feature enabled needs it to be read back.
    pub fn register(
        &mut self,
        name: &str,
        arc_format_change: bool,
        validator: Option<Validator>,
    ) -> Result<Feature, Error> {
        if self.entries.iter().any(|e| e.name == name) {
            return Err(Error::Fatal(format!("Feature {name} registered twice")));
        }
        self.entries.push(Entry {
            name: name.to_owned(),
            enabled: false,
            arc_format_change,
            validator,
        });
        Ok(Feature(Some(self.entries.len() - 1)))
    }

    /// `Features.enabled`. Meaningful once negotiation has completed.
    pub fn enabled(&self, feature: Feature) -> bool {
        feature.0.is_some_and(|i| self.entries[i].enabled)
    }

    /// Whether the feature registered as `name` is enabled (false if there
    /// is none). For codecs, which get the registry rather than handles.
    pub fn enabled_by_name(&self, name: &str) -> bool {
        self.entries.iter().any(|e| e.enabled && e.name == name)
    }

    /// `Features.all`: every registered feature, most recently registered
    /// first (the order OCaml sends them in).
    pub fn all(&self) -> Vec<Id> {
        self.entries.iter().rev().map(|e| e.name.clone()).collect()
    }

    /// `Features.getEnabled`.
    pub fn get_enabled(&self) -> Vec<Id> {
        self.names(|e| e.enabled)
    }

    /// `Features.changingArchiveFormat`: enabled features that the on-disk
    /// archive depends on.
    pub fn changing_archive_format(&self) -> Vec<Id> {
        self.names(|e| e.enabled && e.arc_format_change)
    }

    fn names(&self, keep: impl Fn(&Entry) -> bool) -> Vec<Id> {
        // OCaml folds over a hash table, so the order is unspecified; sort
        // for determinism.
        let mut names: Vec<Id> = self
            .entries
            .iter()
            .filter(|e| keep(e))
            .map(|e| e.name.clone())
            .collect();
        names.sort();
        names
    }

    /// `Features.setEnabled`: enables exactly the registered features in `ids`.
    pub fn set_enabled(&mut self, ids: &[Id]) {
        for e in &mut self.entries {
            e.enabled = ids.contains(&e.name);
        }
    }

    /// `Features.resetEnabled`.
    pub fn reset_enabled(&mut self) {
        self.set_enabled(&[]);
    }

    /// `Features.validate`: runs every validator against `ids`.
    pub fn validate(&self, ids: &[Id], prefs: &Prefs) -> Result<(), Error> {
        let by_name: BTreeMap<_, _> = self.entries.iter().map(|e| (&e.name, e)).collect();
        for (name, e) in by_name {
            let Some(validator) = &e.validator else {
                continue;
            };
            if let Some(err) = validator(ids, ids.contains(name), prefs) {
                return Err(Error::Fatal(format!(
                    "Client and server are incompatible. Setting up feature \"{name}\" \
                     failed with error\n\"{err}\".\n\nIt may be possible to rectify this \
                     by changing the user preferences.\nUltimately, it may require \
                     upgrading either the server or the client."
                )));
            }
        }
        Ok(())
    }

    /// `Features.validateEnabled`.
    pub fn validate_enabled(&self, prefs: &Prefs) -> Result<(), Error> {
        self.validate(&self.get_enabled(), prefs)
    }
}

/// `Features.inter`: the elements of `b` that are also in `a`, in `b`'s order.
pub fn inter(a: &[Id], b: &[Id]) -> Vec<Id> {
    b.iter().filter(|n| a.contains(n)).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_enable_and_query() {
        let mut f = Features::new();
        let moves = f.register("Sync: Moves", false, None).unwrap();
        let xattr = f.register("Sync: xattr", true, None).unwrap();
        assert!(f.register("Sync: Moves", false, None).is_err());
        assert_eq!(f.all(), ["Sync: xattr", "Sync: Moves"]);
        assert!(!f.enabled(moves));

        f.set_enabled(&["Sync: xattr".into(), "unknown".into()]);
        assert!(f.enabled_by_name("Sync: xattr"));
        assert!(!f.enabled_by_name("Sync: Moves") && !f.enabled_by_name("unknown"));
        assert!(!f.enabled(moves) && f.enabled(xattr));
        assert!(!f.enabled(Feature::DUMMY));
        assert_eq!(f.changing_archive_format(), ["Sync: xattr"]);
        f.reset_enabled();
        assert!(f.get_enabled().is_empty());
    }

    #[test]
    fn validators_can_reject_a_set() {
        let mut f = Features::new();
        f.register(
            "Sync: ACL",
            true,
            Some(Box::new(|_, included, _| {
                (!included).then(|| "ACL sync requested but not supported".to_owned())
            })),
        )
        .unwrap();
        assert!(f.validate(&["Sync: ACL".into()], &Prefs::new()).is_ok());
        let err = f.validate(&[], &Prefs::new()).unwrap_err().to_string();
        assert!(
            err.starts_with("Client and server are incompatible. Setting up feature \"Sync: ACL\"")
        );
    }

    #[test]
    fn inter_keeps_the_second_lists_order() {
        let a = ["x".into(), "y".into()];
        let b = ["y".into(), "z".into(), "x".into()];
        assert_eq!(inter(&a, &b), ["y", "x"]);
    }
}
