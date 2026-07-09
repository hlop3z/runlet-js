//! Shared SSRF protection — blocks targets that resolve to private/internal IPs.
//!
//! Used by both trust-sensitive capabilities:
//! - `http` validates a **script-controlled** request URL on every call.
//! - `s3` validates the **operator-configured** endpoint host before signing, so a
//!   presigned URL can never name a local/internal target.
//!
//! Keeping the IP classification here (instead of duplicated per module) means the
//! blocklist stays consistent across capabilities.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

// The connect-time resolver below is `reqwest`-backed, so its imports are gated to the
// in-engine capabilities that install it (`http`/`s3`); the pure-std classifier is always on.
#[cfg(any(feature = "http", feature = "s3"))]
use std::error::Error;
#[cfg(any(feature = "http", feature = "s3"))]
use std::net::SocketAddr;

#[cfg(any(feature = "http", feature = "s3"))]
use std::collections::HashSet;
#[cfg(any(feature = "http", feature = "s3"))]
use std::sync::Arc;

#[cfg(any(feature = "http", feature = "s3"))]
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
#[cfg(any(feature = "http", feature = "s3"))]
use tokio::task;

/// Blocks a host that is — or resolves to — a private/internal IP address.
///
/// Literal IPs are checked directly. Hostnames are resolved (one DNS lookup) and
/// every returned address is checked. DNS failure is not fatal here: the eventual
/// connection will fail on its own.
///
/// When `allow_private` is `true` (server `debug` mode), the check is skipped so
/// localhost / LAN targets work for local testing. Production runs with it `false`.
///
/// # Errors
///
/// Returns an error if the host is, or resolves to, a private/internal address.
pub(crate) fn block_private_ip(host: &str, port: u16, allow_private: bool) -> Result<(), String> {
    if allow_private {
        return Ok(());
    }

    // Literal IP check (no DNS needed).
    if let Ok(addr) = host.parse::<IpAddr>() {
        if is_private_ip(&addr) {
            return Err(format!("requests to private IP {addr} are blocked"));
        }
        return Ok(());
    }

    // Resolve hostname and check every address it points at.
    if let Ok(addrs) = (host, port).to_socket_addrs() {
        for sock_addr in addrs {
            if is_private_ip(&sock_addr.ip()) {
                return Err(format!(
                    "host '{host}' resolves to private IP {}, blocked",
                    sock_addr.ip()
                ));
            }
        }
    }
    // DNS failure will surface later when the actual connection is attempted.
    Ok(())
}

/// Returns `true` if the address is private/internal (SSRF protection).
pub(crate) fn is_private_ip(addr: &IpAddr) -> bool {
    match *addr {
        IpAddr::V4(ip) => is_private_v4(ip),
        IpAddr::V6(ip) => is_private_v6(ip),
    }
}

/// The **inverse** of [`block_private_ip`] — asserts a target is loopback/private (co-located),
/// used by the box-direct local-egress boot guard (byo-capabilities D8): a box-direct binding may
/// only point at a co-located service, so a target that is (or resolves to) any **public** address
/// is rejected. A literal is classified directly; a hostname must resolve and **every** address it
/// yields must be private (fail-closed — a mixed or unresolved host is rejected).
///
/// # Errors
///
/// Returns an error if the host is a public literal, resolves to any public address, or does not
/// resolve at all.
#[cfg(feature = "http")]
pub(crate) fn require_private_target(host: &str, port: u16) -> Result<(), String> {
    if let Ok(addr) = host.parse::<IpAddr>() {
        return if is_private_ip(&addr) {
            Ok(())
        } else {
            Err(format!(
                "box-direct target {addr} is a public address; a remote target must go through a broker"
            ))
        };
    }
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|err| format!("box-direct target '{host}' did not resolve: {err}"))?;
    let mut resolved = false;
    for sock_addr in addrs {
        resolved = true;
        if !is_private_ip(&sock_addr.ip()) {
            return Err(format!(
                "box-direct target '{host}' resolves to public address {}; a remote target must go through a broker",
                sock_addr.ip()
            ));
        }
    }
    if resolved {
        Ok(())
    } else {
        Err(format!(
            "box-direct target '{host}' did not resolve to any address"
        ))
    }
}

/// Returns `true` for non-public IPv6: loopback, unique-local, link-local, multicast, and any
/// form embedding a private IPv4 (v4-mapped, v4-compatible, 6to4, NAT64).
///
/// `std`'s `Ipv6Addr::is_unique_local` / `is_unicast_link_local` are still unstable, so the
/// ranges are matched directly on the segments — an attacker on an IPv6 network reaches
/// internal hosts via ULA (`fd00::…`) or smuggles a private v4 through 6to4 / NAT64, and the
/// previous filter (loopback + v4-mapped only) let all of those through.
fn is_private_v6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    // IPv4-mapped `::ffff:a.b.c.d` — re-check the embedded v4.
    if ip.to_ipv4_mapped().is_some_and(is_private_v4) {
        return true;
    }
    let [seg0, ..] = ip.segments();
    // Unique-local fc00::/7 (covers the common fd00::/8 deployments).
    let unique_local = (seg0 & 0xfe00) == 0xfc00;
    // Link-local fe80::/10 (the IPv6 path to host-local / metadata services).
    let link_local = (seg0 & 0xffc0) == 0xfe80;
    // Multicast ff00::/8 — never a legitimate unicast egress target.
    let multicast = (seg0 & 0xff00) == 0xff00;
    unique_local
        || link_local
        || multicast
        || v6_v4_compatible_private(ip)
        || v6_embeds_private_v4(ip)
}

/// Returns `true` for the deprecated IPv4-compatible IPv6 form `::a.b.c.d` (`::/96`, high 96
/// bits zero) whose embedded IPv4 is private. `::` (unspecified) and `::1` (loopback) are
/// classified before this runs, so only a real embedded address reaches it — the same
/// unwrap-and-re-check `to_ipv4_mapped` does for the `::ffff:` form.
const fn v6_v4_compatible_private(ip: Ipv6Addr) -> bool {
    let oct = ip.octets();
    let high_96_zero = oct[0] == 0
        && oct[1] == 0
        && oct[2] == 0
        && oct[3] == 0
        && oct[4] == 0
        && oct[5] == 0
        && oct[6] == 0
        && oct[7] == 0
        && oct[8] == 0
        && oct[9] == 0
        && oct[10] == 0
        && oct[11] == 0;
    high_96_zero && is_private_v4(Ipv4Addr::new(oct[12], oct[13], oct[14], oct[15]))
}

/// Returns `true` if `ip` carries a private IPv4 inside a public-looking IPv6 literal via
/// 6to4 (`2002:a.b.c.d::/48`) or NAT64 (`64:ff9b::a.b.c.d`) — both can smuggle a private v4
/// past a naïve "looks public" check, so they are classified by the embedded address.
const fn v6_embeds_private_v4(ip: Ipv6Addr) -> bool {
    let [seg0, seg1, ..] = ip.segments();
    let [
        _,
        _,
        sx0,
        sx1,
        sx2,
        sx3,
        _,
        _,
        _,
        _,
        _,
        _,
        nx0,
        nx1,
        nx2,
        nx3,
    ] = ip.octets();
    let sixtofour = seg0 == 0x2002 && is_private_v4(Ipv4Addr::new(sx0, sx1, sx2, sx3));
    let nat64 =
        seg0 == 0x0064 && seg1 == 0xff9b && is_private_v4(Ipv4Addr::new(nx0, nx1, nx2, nx3));
    sixtofour || nat64
}

/// Returns `true` for loopback, private, link-local, multicast, reserved, and other
/// non-public IPv4.
const fn is_private_v4(ip: Ipv4Addr) -> bool {
    let [oct_a, oct_b, _, _] = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_unspecified()
        || (oct_a == 100 && oct_b >= 64 && oct_b <= 127) // CGNAT 100.64.0.0/10
        || oct_a >= 224 // multicast 224.0.0.0/4 + reserved/future 240.0.0.0/4
}

// -- Connect-time pinning (shared by `http` and `s3`) -----------------------
//
// `reqwest`-backed, so each item is gated to the in-engine capabilities that install it. The
// pure-std classifier above is always compiled (the mux uses it for `ScriptControlled` caps).

/// A `reqwest` DNS resolver that drops every address failing the SSRF classifier **at the
/// lookup reqwest connects with**, so a hostname can't resolve public for a pre-check and
/// private for the actual connection (DNS rebinding). [`block_private_ip`] stays as a
/// fast-fail for literal IPs and a clean in-band error; this is the authoritative
/// connect-time backstop. In `debug` mode (`allow_private`) the filter is skipped.
///
/// Shared by both script-/operator-controlled capabilities: `http` installs it on its
/// request client and `s3` on its list/delete/send client, so the same pinning guarantee
/// covers every capability that opens an outbound connection.
#[cfg(any(feature = "http", feature = "s3"))]
pub(crate) struct SsrfResolver {
    /// When `true` (server `debug`), private/internal addresses are allowed (local testing).
    allow_private: bool,
    /// Lowercased host names explicitly allowlisted with a port in `http.allowed_hosts` (the D6
    /// targeted local bypass): resolving one of these permits its private/internal address even
    /// when `allow_private` is off, so a co-located service (`localhost:8000`) is reachable in
    /// production without the blanket `debug` relax. Empty for `s3` (no per-host bypass).
    bypass_hosts: Arc<HashSet<String>>,
}

#[cfg(any(feature = "http", feature = "s3"))]
impl SsrfResolver {
    /// Builds a resolver honoring the `allow_private` (debug) relaxation, with no per-host bypass.
    pub(crate) fn new(allow_private: bool) -> Self {
        Self {
            allow_private,
            bypass_hosts: Arc::new(HashSet::new()),
        }
    }

    /// Builds a resolver that additionally permits the private addresses of `bypass_hosts` (the D6
    /// targeted local allowlist) — each entry a lowercased host name explicitly allowlisted with a
    /// port. Used by the `http` capability; `s3` uses [`Self::new`].
    #[cfg(feature = "http")]
    pub(crate) const fn with_bypass(
        allow_private: bool,
        bypass_hosts: Arc<HashSet<String>>,
    ) -> Self {
        Self {
            allow_private,
            bypass_hosts,
        }
    }
}

#[cfg(any(feature = "http", feature = "s3"))]
impl Resolve for SsrfResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        // A host explicitly allowlisted with a port bypasses the private-IP filter at connect time
        // too (D6), else the pre-check would pass while the resolver still dropped the local address.
        let allow_private = self.allow_private || self.bypass_hosts.contains(&host.to_lowercase());
        // Run the blocking `getaddrinfo` off the reactor (the blocking client is a
        // current-thread runtime) so the request timeout can still fire while DNS is in flight.
        Box::pin(async move {
            match task::spawn_blocking(move || resolve_filtered(&host, allow_private)).await {
                Ok(result) => result,
                Err(join_err) => Err(join_err.into()),
            }
        })
    }
}

/// Resolves `host` and keeps only addresses the SSRF classifier permits. Returns an error if
/// the host doesn't resolve or every address is private/internal (fail closed — never hand
/// reqwest an empty or unfiltered set). Port `0` is a placeholder; reqwest substitutes the
/// URL's port.
#[cfg(any(feature = "http", feature = "s3"))]
pub(crate) fn resolve_filtered(
    host: &str,
    allow_private: bool,
) -> Result<Addrs, Box<dyn Error + Send + Sync>> {
    let mut kept: Vec<SocketAddr> = Vec::new();
    for addr in (host, 0_u16).to_socket_addrs()? {
        if allow_private || !is_private_ip(&addr.ip()) {
            kept.push(addr);
        }
    }
    if kept.is_empty() {
        return Err(format!("host '{host}' has no public address (SSRF-filtered)").into());
    }
    Ok(Box::new(kept.into_iter()))
}

#[cfg(test)]
mod tests {
    //! SSRF classification — the IPv4 ranges plus the IPv6 ranges that the v4-mapped-only
    //! filter used to miss (ULA, link-local, 6to4 / NAT64 embedded private v4), the
    //! deprecated IPv4-compatible form, multicast, and reserved/future ranges — plus the
    //! shared connect-time pinning filter.

    #[cfg(any(feature = "http", feature = "s3"))]
    use super::resolve_filtered;
    use super::{block_private_ip, is_private_ip};
    use std::net::IpAddr;

    /// Parses a literal into an `IpAddr` for the table-driven cases.
    fn ip(text: &str) -> IpAddr {
        text.parse()
            .unwrap_or_else(|_err| unreachable!("test literal must parse: {text}"))
    }

    /// IPv4 loopback / private / link-local / CGNAT are all classified private.
    #[test]
    fn v4_private_ranges_blocked() {
        for literal in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.1.1",
            "172.16.0.1",
            "169.254.169.254", // cloud metadata
            "100.64.0.1",      // CGNAT
            "0.0.0.0",
        ] {
            assert!(is_private_ip(&ip(literal)), "must be private: {literal}");
        }
    }

    /// A routable public v4 is allowed.
    #[test]
    fn v4_public_allowed() {
        assert!(!is_private_ip(&ip("8.8.8.8")), "public v4 is allowed");
        assert!(!is_private_ip(&ip("1.1.1.1")), "public v4 is allowed");
    }

    /// IPv6 loopback, unique-local, and link-local are classified private — the ranges the
    /// old filter let through.
    #[test]
    fn v6_internal_ranges_blocked() {
        for literal in [
            "::1",              // loopback
            "::",               // unspecified
            "fd00::1",          // ULA (fc00::/7)
            "fc00::1",          // ULA boundary
            "fe80::1",          // link-local
            "febf::1",          // link-local boundary
            "::ffff:127.0.0.1", // v4-mapped loopback
            "::ffff:10.0.0.1",  // v4-mapped private
        ] {
            assert!(is_private_ip(&ip(literal)), "must be private: {literal}");
        }
    }

    /// A private v4 smuggled through 6to4 or NAT64 is unwrapped and blocked.
    #[test]
    fn v6_embedded_private_v4_blocked() {
        // 6to4 of 192.168.1.1 → 2002:c0a8:0101::
        assert!(
            is_private_ip(&ip("2002:c0a8:0101::1")),
            "6to4 of a private v4 is blocked"
        );
        // NAT64 of 127.0.0.1 → 64:ff9b::7f00:1
        assert!(
            is_private_ip(&ip("64:ff9b::7f00:1")),
            "NAT64 of loopback is blocked"
        );
    }

    /// A genuine public v6 (and a 6to4/NAT64 wrapping a *public* v4) is allowed.
    #[test]
    fn v6_public_allowed() {
        assert!(
            !is_private_ip(&ip("2606:4700:4700::1111")),
            "public v6 is allowed"
        );
        // 6to4 of 8.8.8.8 → 2002:0808:0808:: must NOT be blocked.
        assert!(
            !is_private_ip(&ip("2002:0808:0808::1")),
            "6to4 of a public v4 is allowed"
        );
    }

    /// The deprecated IPv4-compatible form `::a.b.c.d` (no `ffff`) is unwrapped and blocked
    /// when the embedded v4 is private — the range `to_ipv4_mapped` misses.
    #[test]
    fn v6_v4_compatible_private_blocked() {
        // ::7f00:1 == ::127.0.0.1 (IPv4-compatible loopback).
        assert!(
            is_private_ip(&ip("::7f00:1")),
            "IPv4-compatible loopback is blocked"
        );
        // ::a00:1 == ::10.0.0.1 (IPv4-compatible private).
        assert!(
            is_private_ip(&ip("::a00:1")),
            "IPv4-compatible private is blocked"
        );
        // A public v4 in the compatible form stays allowed (consistent with 6to4/NAT64).
        assert!(
            !is_private_ip(&ip("::808:808")),
            "IPv4-compatible public v4 is allowed"
        );
    }

    /// Multicast and reserved/future ranges are blocked in both families.
    #[test]
    fn multicast_and_reserved_blocked() {
        for literal in [
            "224.0.0.1", // IPv4 multicast 224.0.0.0/4
            "239.1.2.3", // IPv4 multicast upper bound
            "240.0.0.1", // IPv4 reserved/future 240.0.0.0/4
            "ff02::1",   // IPv6 link-local all-nodes multicast
            "ff00::1",   // IPv6 multicast ff00::/8 boundary
        ] {
            assert!(is_private_ip(&ip(literal)), "must be blocked: {literal}");
        }
    }

    /// The SSRF filter keeps a public literal, rejects a private one, and honors the debug
    /// relaxation. Literal IPs resolve without network, so this is hermetic.
    #[cfg(any(feature = "http", feature = "s3"))]
    #[test]
    fn resolve_filtered_drops_private_addresses() {
        assert_eq!(
            resolve_filtered("8.8.8.8", false).map(Iterator::count).ok(),
            Some(1),
            "a public literal survives the filter"
        );
        assert!(
            resolve_filtered("127.0.0.1", false).is_err(),
            "a private literal is filtered to empty → error (fail closed)"
        );
        assert_eq!(
            resolve_filtered("127.0.0.1", true)
                .map(Iterator::count)
                .ok(),
            Some(1),
            "debug relaxation lets a private literal through"
        );
    }

    /// `allow_private` (debug mode) short-circuits the block for local testing.
    #[test]
    fn debug_relaxation_allows_private() {
        assert!(
            block_private_ip("127.0.0.1", 80, true).is_ok(),
            "debug relaxes v4 loopback"
        );
        assert!(
            block_private_ip("fd00::1", 80, true).is_ok(),
            "debug relaxes v6 ULA"
        );
        assert!(
            block_private_ip("10.0.0.1", 5432, false).is_err(),
            "production blocks private v4"
        );
        assert!(
            block_private_ip("fe80::1", 80, false).is_err(),
            "production blocks v6 link-local"
        );
    }
}
