//! Opaque identifiers for domain objects.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Creates a fresh random identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Wraps an identifier that was persisted earlier.
            #[must_use]
            pub fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// Returns the underlying value, for storage keys and logging.
            #[must_use]
            pub fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, formatter)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "{}({})", stringify!($name), self.0)
            }
        }
    };
}

define_id! {
    /// Identifies a stored SSH host configuration.
    HostId
}

define_id! {
    /// Identifies one terminal session, local or remote.
    SessionId
}

define_id! {
    /// Identifies one file transfer task.
    TransferId
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_unique_and_round_trip_through_uuid() {
        let first = SessionId::new();
        let second = SessionId::new();
        assert_ne!(first, second);

        let restored = SessionId::from_uuid(first.as_uuid());
        assert_eq!(first, restored);
        assert_eq!(first.to_string(), restored.to_string());
        assert!(!first.to_string().starts_with("SessionId"));
    }

    #[test]
    fn identifiers_serialize_as_uuid_strings() {
        let id = HostId::new();
        let encoded = serde_json::to_string(&id).expect("host id serializes");
        assert_eq!(encoded, format!("\"{}\"", id.as_uuid()));
    }
}
