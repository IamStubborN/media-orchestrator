use std::{
    fmt,
    net::{Ipv4Addr, Ipv6Addr},
};

use url::{Host, Url};

use crate::{RezkaError, redaction::sanitize_provider_text};

pub struct SecretMediaUrl(Url);

impl SecretMediaUrl {
    pub fn new(url: Url) -> Result<Self, RezkaError> {
        validate_public_https_url(&url)?;
        Ok(Self(url))
    }

    pub fn with_url<R>(&self, operation: impl FnOnce(&Url) -> R) -> R {
        operation(&self.0)
    }
}

impl fmt::Debug for SecretMediaUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretMediaUrl([REDACTED])")
    }
}

impl fmt::Display for SecretMediaUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

pub struct SecretSubtitleUrl(Url);

impl SecretSubtitleUrl {
    pub fn new(url: Url) -> Result<Self, RezkaError> {
        validate_public_https_url(&url)?;
        Ok(Self(url))
    }

    pub fn with_url<R>(&self, operation: impl FnOnce(&Url) -> R) -> R {
        operation(&self.0)
    }
}

impl fmt::Debug for SecretSubtitleUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretSubtitleUrl([REDACTED])")
    }
}

impl fmt::Display for SecretSubtitleUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

pub struct PublicImageUrl(Url);

impl PublicImageUrl {
    pub fn new(url: Url) -> Result<Self, RezkaError> {
        validate_public_https_url(&url)?;
        Ok(Self(url))
    }

    #[must_use]
    pub fn url(&self) -> &Url {
        &self.0
    }
}

impl fmt::Debug for PublicImageUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PublicImageUrl([REDACTED])")
    }
}

fn validate_public_https_url(url: &Url) -> Result<(), RezkaError> {
    let valid = url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && url.host().as_ref().is_some_and(is_public_host);

    if valid {
        Ok(())
    } else {
        Err(RezkaError::ProviderResponseInvalid {
            context: sanitize_provider_text("invalid public HTTPS URL"),
        })
    }
}

fn is_public_host(host: &Host<&str>) -> bool {
    match host {
        Host::Domain(domain) => {
            let normalized = domain.to_ascii_lowercase();
            normalized != "localhost" && !normalized.ends_with(".localhost")
        }
        Host::Ipv4(address) => is_global_ipv4(*address),
        Host::Ipv6(address) => is_global_ipv6(*address),
    }
}

fn is_global_ipv4(address: Ipv4Addr) -> bool {
    let [first, second, third, _] = address.octets();

    !address.is_unspecified()
        && !address.is_loopback()
        && !address.is_private()
        && !address.is_link_local()
        && !address.is_broadcast()
        && first != 0
        && first < 224
        && !(first == 100 && (64..=127).contains(&second))
        && !(first == 192 && second == 0 && third == 0)
        && !(first == 192 && second == 0 && third == 2)
        && !(first == 192 && second == 88 && third == 99)
        && !(first == 192 && second == 168)
        && !(first == 198 && (second == 18 || second == 19))
        && !(first == 198 && second == 51 && third == 100)
        && !(first == 203 && second == 0 && third == 113)
}

fn is_global_ipv6(address: Ipv6Addr) -> bool {
    if let Some(mapped_ipv4) = address.to_ipv4_mapped() {
        return is_global_ipv4(mapped_ipv4);
    }

    !address.is_multicast() && iana_ipv6_special_purpose_globally_reachable(address).unwrap_or(true)
}

fn iana_ipv6_special_purpose_globally_reachable(address: Ipv6Addr) -> Option<bool> {
    let segments = address.segments();

    match segments {
        [0, 0, 0, 0, 0, 0, 0, 0] | [0, 0, 0, 0, 0, 0, 0, 1] => Some(false),
        [0x0064, 0xff9b, 0, 0, 0, 0, _, _] => Some(true),
        [0x0064, 0xff9b, 1, _, _, _, _, _]
        | [0x0100, 0, 0, 0, _, _, _, _]
        | [0x0100, 0, 0, 1, _, _, _, _]
        | [0x2001, 0x0db8, _, _, _, _, _, _]
        | [0x2002, _, _, _, _, _, _, _]
        | [0x5f00, _, _, _, _, _, _, _]
        | [0xfc00..=0xfdff, _, _, _, _, _, _, _]
        | [0xfe80..=0xfebf, _, _, _, _, _, _, _] => Some(false),
        [0x3fff, second, _, _, _, _, _, _] if second < 0x1000 => Some(false),
        [0x2001, second, _, _, _, _, _, _] if second < 0x0200 => Some(matches!(
            segments,
            [0x2001, 1, 0, 0, 0, 0, 0, 1..=3]
                | [0x2001, 3, _, _, _, _, _, _]
                | [0x2001, 4, 0x0112, _, _, _, _, _]
                | [0x2001, 0x0020..=0x003f, _, _, _, _, _, _]
        )),
        [0x2620, 0x004f, 0x8000, _, _, _, _, _] => Some(true),
        _ => None,
    }
}
