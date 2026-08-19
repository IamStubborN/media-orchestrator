use reqwest::StatusCode;
use secrecy::ExposeSecret;
use serde::Deserialize;

use crate::{
    RezkaError, redaction::sanitize_provider_text, session::RezkaCredentials, transport::Transport,
};

const LOGIN_PATH: &str = "/ajax/login/";
const SESSION_COOKIE: &str = "PHPSESSID";

#[derive(Deserialize)]
struct LoginResponse {
    success: bool,
}

pub(crate) async fn login(
    transport: &mut Transport,
    credentials: &RezkaCredentials,
) -> Result<(), RezkaError> {
    let referer = transport.selected_origin().clone();
    if !credential_transport_allowed(&referer) {
        return Err(RezkaError::Configuration {
            message: "Rezka credentials require HTTPS",
        });
    }
    let url = referer.join(LOGIN_PATH).map_err(|_| invalid_response())?;
    let form = [
        ("login_name", credentials.username.expose_secret()),
        ("login_password", credentials.password.expose_secret()),
        ("login_not_save", "0"),
        ("login", "submit"),
    ];
    let response = transport.post_form_first(url, Some(referer), &form).await?;
    let stored_session_cookie = response.stored_cookie_names().contains(SESSION_COOKIE);

    if response.status.is_redirection() {
        return stored_session_cookie
            .then_some(())
            .ok_or_else(invalid_response);
    }
    if response.status != StatusCode::OK {
        return Err(invalid_response());
    }

    if !response.body.trim().eq_ignore_ascii_case("redirect") {
        let parsed: LoginResponse =
            serde_json::from_str(&response.body).map_err(|_| invalid_response())?;
        if !parsed.success {
            return Err(RezkaError::AuthenticationFailed {
                context: sanitize_provider_text("DLE login rejected"),
            });
        }
    }

    stored_session_cookie
        .then_some(())
        .ok_or_else(invalid_response)
}

fn credential_transport_allowed(origin: &url::Url) -> bool {
    if origin.scheme() == "https" {
        return true;
    }
    if origin.scheme() != "http" {
        return false;
    }

    origin.host().is_some_and(|host| match host {
        url::Host::Ipv4(address) => address.is_loopback(),
        url::Host::Ipv6(address) => address.is_loopback(),
        url::Host::Domain(_) => false,
    })
}

fn invalid_response() -> RezkaError {
    RezkaError::ProviderResponseInvalid {
        context: sanitize_provider_text("DLE login response invalid"),
    }
}

#[cfg(test)]
mod tests {
    use super::credential_transport_allowed;

    #[test]
    fn credential_transport_allows_https_and_exact_ip_loopback_http_only() {
        for allowed in [
            "https://rezka.test/",
            "http://127.0.0.1:8080/",
            "http://127.255.255.254:8080/",
            "http://[::1]:8080/",
        ] {
            assert!(credential_transport_allowed(
                &url::Url::parse(allowed).unwrap()
            ));
        }
        for rejected in [
            "http://localhost:8080/",
            "http://192.0.2.1/",
            "ftp://127.0.0.1/",
        ] {
            assert!(!credential_transport_allowed(
                &url::Url::parse(rejected).unwrap()
            ));
        }
    }
}
