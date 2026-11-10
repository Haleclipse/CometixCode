//! Peer address parsing, separate from peer-registry and transport state.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerAddressScheme {
    Uds,
    Unsupported,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerAddress {
    pub scheme: PeerAddressScheme,
    pub target: String,
}

pub fn parse_address(to: &str) -> PeerAddress {
    if let Some(target) = to.strip_prefix("uds:") {
        return PeerAddress {
            scheme: PeerAddressScheme::Uds,
            target: target.to_string(),
        };
    }
    // Bare socket paths remain local peer addresses, including paths with colons.
    if to.starts_with('/') {
        return PeerAddress {
            scheme: PeerAddressScheme::Uds,
            target: to.to_string(),
        };
    }
    let has_scheme = to.split_once(':').is_some_and(|(scheme, _)| {
        scheme.starts_with(|ch: char| ch.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
    });
    PeerAddress {
        scheme: if has_scheme {
            PeerAddressScheme::Unsupported
        } else {
            PeerAddressScheme::Other
        },
        target: to.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_address_preserves_supported_peers_and_plain_names() {
        for (input, scheme, target) in [
            ("uds:/tmp/sock", PeerAddressScheme::Uds, "/tmp/sock"),
            ("uds:", PeerAddressScheme::Uds, ""),
            (
                "/tmp/legacy.sock",
                PeerAddressScheme::Uds,
                "/tmp/legacy.sock",
            ),
            (
                "/tmp/peer:socket",
                PeerAddressScheme::Uds,
                "/tmp/peer:socket",
            ),
            ("researcher", PeerAddressScheme::Other, "researcher"),
            ("*", PeerAddressScheme::Other, "*"),
        ] {
            assert_eq!(
                parse_address(input),
                PeerAddress {
                    scheme,
                    target: target.to_string()
                }
            );
        }
    }

    #[test]
    fn parse_address_rejects_all_unsupported_schemes_generically() {
        for input in [
            "bridge:abc123",
            "bridge:",
            "https://peer",
            "UDS:/tmp/sock",
            "custom+v1.test:peer",
        ] {
            assert_eq!(parse_address(input).scheme, PeerAddressScheme::Unsupported);
        }
        for input in ["", ":peer", "1name:peer", "plain name:peer"] {
            assert_eq!(parse_address(input).scheme, PeerAddressScheme::Other);
        }
    }
}
