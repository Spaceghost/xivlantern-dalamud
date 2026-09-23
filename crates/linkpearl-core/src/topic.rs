//! Deriving a gossip topic from a scope and a key.
//!
//! Two clients that already share a secret — a party, an FC id, a world name —
//! can meet without a directory and without anyone publishing anything. The
//! topic is a blake3 hash, so the key never travels.

use iroh_gossip::proto::TopicId;

use crate::ticket::Scope;

/// `blake3(app_id || 0x1f || scope || 0x1f || key)`.
///
/// The unit separator means `("zone", "153")` and `("zon", "e153")` cannot
/// collide, and the app id keeps two different mods apart even if they pick the
/// same key.
pub fn derive(app_id: &str, scope: Scope, key: &str) -> TopicId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(app_id.as_bytes());
    hasher.update(&[0x1f]);
    hasher.update(scope.as_str().as_bytes());
    hasher.update(&[0x1f]);
    hasher.update(key.as_bytes());
    TopicId::from_bytes(*hasher.finalize().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivation_is_stable_and_separated() {
        let a = derive("app/1", Scope::Zone, "153");
        let b = derive("app/1", Scope::Zone, "153");
        assert_eq!(a, b, "same inputs must give the same topic");

        assert_ne!(a, derive("app/2", Scope::Zone, "153"), "app id separates");
        assert_ne!(a, derive("app/1", Scope::World, "153"), "scope separates");
        assert_ne!(a, derive("app/1", Scope::Zone, "154"), "key separates");
    }

    #[test]
    fn key_boundary_is_separated() {
        // The separator is what keeps a scope/key boundary from sliding: these
        // two would concatenate to the same bytes without it.
        assert_ne!(
            derive("app/1", Scope::Custom, "x"),
            derive("app/1", Scope::Custom, "\u{1f}x")
        );
    }
}
