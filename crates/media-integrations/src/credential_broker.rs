use std::{net::IpAddr, time::Duration};

use futures_util::StreamExt as _;
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};
use url::Url;

const MAX_RESPONSE_BYTES: usize = 16 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum CredentialBrokerError {
    #[error("credential broker configuration is invalid")]
    Configuration,
    #[error("credential broker request failed")]
    Transport,
    #[error("credential broker rejected the request")]
    Rejected,
    #[error("credential broker response is invalid")]
    InvalidResponse,
}

pub struct CredentialBrokerConfig {
    endpoint: Url,
    token: SecretString,
    timeout: Duration,
}

impl CredentialBrokerConfig {
    pub fn new(
        mut base_url: Url,
        token: SecretString,
        timeout: Duration,
        private_http_hosts: &[String],
    ) -> Result<Self, CredentialBrokerError> {
        if base_url.host_str().is_none()
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
            || token.expose_secret().trim().is_empty()
            || timeout.is_zero()
            || !transport_allowed(&base_url, private_http_hosts)
        {
            return Err(CredentialBrokerError::Configuration);
        }
        if !base_url.path().ends_with('/') {
            base_url.set_path(&format!("{}/", base_url.path()));
        }
        let endpoint = base_url
            .join("v1/command")
            .map_err(|_| CredentialBrokerError::Configuration)?;
        Ok(Self {
            endpoint,
            token,
            timeout,
        })
    }
}

pub struct CredentialBrokerClient {
    client: reqwest::Client,
    config: CredentialBrokerConfig,
}

impl CredentialBrokerClient {
    pub fn new(config: CredentialBrokerConfig) -> Result<Self, CredentialBrokerError> {
        let client = reqwest::Client::builder()
            .connect_timeout(config.timeout)
            .timeout(config.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| CredentialBrokerError::Configuration)?;
        Ok(Self { client, config })
    }

    pub async fn resolve(
        &self,
        request_id: &str,
    ) -> Result<BrokerCredentials, CredentialBrokerError> {
        if request_id.trim().is_empty() || request_id.len() > 256 {
            return Err(CredentialBrokerError::Configuration);
        }
        let response = self
            .client
            .post(self.config.endpoint.clone())
            .bearer_auth(self.config.token.expose_secret())
            .json(&BrokerRequest {
                command: "credential_resolve",
                argument: request_id,
            })
            .send()
            .await
            .map_err(|_| CredentialBrokerError::Transport)?;
        if !response.status().is_success() {
            return Err(CredentialBrokerError::Rejected);
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(CredentialBrokerError::InvalidResponse);
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| CredentialBrokerError::Transport)?;
            if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(CredentialBrokerError::InvalidResponse);
            }
            body.extend_from_slice(&chunk);
        }
        let response: BrokerResponse =
            serde_json::from_slice(&body).map_err(|_| CredentialBrokerError::InvalidResponse)?;
        let returned_url =
            Url::parse(&response.url).map_err(|_| CredentialBrokerError::InvalidResponse)?;
        if response.username.trim().is_empty()
            || response.username.len() > 256
            || response.password.is_empty()
            || response.password.len() > 1024
            || returned_url.scheme() != "https"
            || returned_url.host_str().is_none()
            || !returned_url.username().is_empty()
            || returned_url.password().is_some()
        {
            return Err(CredentialBrokerError::InvalidResponse);
        }
        Ok(BrokerCredentials {
            username: response.username,
            password: SecretString::from(response.password),
        })
    }
}

#[derive(Serialize)]
struct BrokerRequest<'a> {
    command: &'static str,
    argument: &'a str,
}

#[derive(Deserialize)]
struct BrokerResponse {
    username: String,
    password: String,
    url: String,
}

pub struct BrokerCredentials {
    pub username: String,
    pub password: SecretString,
}

fn transport_allowed(url: &Url, private_http_hosts: &[String]) -> bool {
    if url.scheme() == "https" {
        return true;
    }
    if url.scheme() != "http" {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(is_private_ip)
        || private_http_hosts
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(host))
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
    }
}

impl std::fmt::Debug for CredentialBrokerClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CredentialBrokerClient { config: [REDACTED] }")
    }
}
