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

/// One field per foreign type that maps to a standard library type (features `url`,
/// `uuid`, `semver` of `novafile-schema`).
#[derive(Debug, PartialEq, Serialize, Deserialize, NovaSchema)]
struct Foreign {
    url: url::Url,
    id: uuid::Uuid,
    version: semver::Version,
}

/// A URL built by `url::Url` from arbitrary text: special, other and opaque schemes; ASCII,
/// Unicode, IPv4 and IPv6 hosts; ports; and paths, queries and fragments of any characters,
/// including those RFC 3986 forbids. `std.Url` must accept whatever `Url` writes for them.
fn url() -> impl Strategy<Value = url::Url> {
    let host = prop_oneof![
        "[A-Za-z0-9]([A-Za-z0-9-]{0,6}[A-Za-z0-9])?(\\.[A-Za-z]{2,4}){0,2}",
        "\\p{L}{1,6}\\.[a-z]{2,3}",
        any::<std::net::Ipv4Addr>().prop_map(|ip| ip.to_string()),
        any::<Ipv6Addr>().prop_map(|ip| format!("[{ip}]")),
    ];
    (
        prop::sample::select(vec!["http", "https", "ws", "wss", "ftp", "file", "git+ssh", "x-app", "mailto", "data"]),
        any::<bool>(),
        host,
        prop::option::of(any::<u16>()),
        "\\PC{0,16}",
    )
        .prop_filter_map("Url rejects the input", |(scheme, slashes, host, port, rest)| {
            let port = port.map(|p| format!(":{p}")).unwrap_or_default();
            let text = if slashes {
                format!("{scheme}://{host}{port}/{rest}")
            } else {
                format!("{scheme}:{rest}")
            };
            url::Url::parse(&text).ok()
        })
}

/// A `SemVer` pre-release or build identifier list.
fn idents(numeric_ok: bool) -> impl Strategy<Value = String> {
    let ident = if numeric_ok {
        "(0|[1-9][0-9]{0,3}|[0-9]{0,2}[A-Za-z-][0-9A-Za-z-]{0,4})"
    } else {
        "[0-9A-Za-z-]{1,6}"
    };
    prop::collection::vec(ident, 0..3).prop_map(|v| v.join("."))
}

prop_compose! {
    fn foreign()(
        url in url(),
        id in any::<u128>(),
        (major, minor, patch) in (any::<u64>(), any::<u64>(), any::<u64>()),
        pre in idents(true),
        build in idents(false),
    ) -> Foreign {
        let mut version = semver::Version::new(major, minor, patch);
        version.pre = semver::Prerelease::new(&pre).unwrap();
        version.build = semver::BuildMetadata::new(&build).unwrap();
        Foreign { url, id: uuid::Uuid::from_u128(id), version }
    }
}

proptest! {
    #[test]
    fn foreign_types_round_trip_through_the_standard_library(f in foreign()) {
        let schema = Foreign::novafile_schema();
        let text = novafile_serde::to_string(&f).unwrap();
        let back: Foreign = novafile_serde::from_str_with_schema(&text, &schema)
            .map_err(|e| TestCaseError::fail(format!("{e}\n{text}")))?;
        prop_assert_eq!(back, f);
    }
}
