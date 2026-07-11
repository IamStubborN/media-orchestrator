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

    let parsed: LoginResponse =
        serde_json::from_str(&response.body).map_err(|_| invalid_response())?;
    if !parsed.success {
        return Err(RezkaError::AuthenticationFailed {
            context: sanitize_provider_text("DLE login rejected"),
        });
    }

    stored_session_cookie
        .then_some(())
        .ok_or_else(invalid_response)
}

fn invalid_response() -> RezkaError {
    RezkaError::ProviderResponseInvalid {
        context: sanitize_provider_text("DLE login response invalid"),
    }
}
