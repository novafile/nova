//! `#[derive(NovaSchema)]` and serde through the umbrella crate alone.

use nova::NovaSchema;
use serde::{Deserialize, Serialize};

/// A service, derived without a direct `novafile-schema` dependency.
#[derive(Serialize, Deserialize, NovaSchema)]
#[nova(name = "infrastructure", version = "1.0.0")]
struct Service {
    /// The service key.
    #[nova(key, id = 1)]
    id: String,

    /// The listening port.
    #[nova(id = 2, range = "1..65535")]
    port: u16,

    /// When the service was last deployed.
    deployed_at: nova::value::Timestamp,
}

#[test]
fn derive_resolves_the_schema_crate_through_the_umbrella() {
    let schema = Service::novafile_schema();
    assert_eq!(schema.name.as_deref(), Some("infrastructure"));

    let svc = Service {
        id: "api".into(),
        port: 8080,
        deployed_at: nova::value::Timestamp::parse("2026-11-17T20:00:00Z").unwrap(),
    };
    let text = nova::serde::to_string(&svc).unwrap();
    let back: Service = nova::serde::from_str(&text).unwrap();
    assert_eq!((back.id.as_str(), back.port), ("api", 8080));
}
