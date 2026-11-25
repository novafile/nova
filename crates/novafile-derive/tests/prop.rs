//! Property tests: values of the std types that `NovaSchema` maps to `string` serialize as
//! strings the derived schema accepts, and deserialize back unchanged.

use novafile_schema::NovaSchema;
use proptest::prelude::*;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::path::PathBuf;

/// One field per std type with a `string` mapping.
#[derive(Debug, PartialEq, Serialize, Deserialize, NovaSchema)]
#[nova(name = "example.org/net", version = "1.0.0")]
struct Net {
    ip: IpAddr,
    v6: Ipv6Addr,
    sock: SocketAddr,
    sock4: SocketAddrV4,
    sock6: SocketAddrV6,
    paths: Vec<PathBuf>,
}

/// A v6 socket address without flow info: `Display`, and so serde's string form, drops it.
fn socket_v6() -> impl Strategy<Value = SocketAddrV6> {
    (any::<Ipv6Addr>(), any::<u16>(), any::<u32>()).prop_map(|(ip, port, scope)| SocketAddrV6::new(ip, port, 0, scope))
}

/// A UTF-8 path, absolute or relative; serde cannot write other paths as strings.
fn path() -> impl Strategy<Value = PathBuf> {
    (any::<bool>(), prop::collection::vec("[^/\\x00]{1,8}", 0..4)).prop_map(|(abs, parts)| {
        let rel = parts.join("/");
        PathBuf::from(if abs { format!("/{rel}") } else { rel })
    })
}

prop_compose! {
    fn net()(
        ip in any::<IpAddr>(),
        v6 in any::<Ipv6Addr>(),
        sock4 in any::<SocketAddrV4>(),
        sock6 in socket_v6(),
        v4_sock: bool,
        paths in prop::collection::vec(path(), 0..3),
    ) -> Net {
        let sock = if v4_sock { SocketAddr::V4(sock4) } else { SocketAddr::V6(sock6) };
        Net { ip, v6, sock, sock4, sock6, paths }
    }
}

proptest! {
    #[test]
    fn std_types_round_trip_through_the_derived_schema(n in net()) {
        let schema = Net::novafile_schema();
        let text = novafile_serde::to_string(&n).unwrap();
        let back: Net = novafile_serde::from_str_with_schema(&text, &schema)
            .map_err(|e| TestCaseError::fail(format!("{e}\n{text}")))?;
        prop_assert_eq!(back, n);
    }
}
