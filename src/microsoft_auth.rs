//! Microsoft → Xbox Live → Minecraft Services flow. No application ID or tokens are bundled.
//! Never log raw HTTP errors, URLs containing authorization codes, or session objects.
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, io};

const TOKEN_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
const AUTHORIZE_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize";
const XBL_URL: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XSTS_URL: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const MC_LOGIN: &str = "https://api.minecraftservices.com/authentication/login_with_xbox";
const ENTITLEMENTS: &str = "https://api.minecraftservices.com/entitlements/mcstore";
const PROFILE: &str = "https://api.minecraftservices.com/minecraft/profile";
fn rejected(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

/// A transient PKCE request. The verifier and state must never appear in logs.
pub struct AuthRequest {
    pub url: String,
    redirect: String,
    state: String,
    verifier: String,
}
impl AuthRequest {
    pub fn redirect_uri(&self) -> &str {
        &self.redirect
    }
    pub fn accept(&self, redirect_url: &str) -> io::Result<String> {
        let url = reqwest::Url::parse(redirect_url)
            .map_err(|_| rejected("Invalid authorization redirect"))?;
        let callback = reqwest::Url::parse(&self.redirect)
            .map_err(|_| rejected("Invalid expected callback"))?;
        if url.scheme() != callback.scheme()
            || url.host_str() != callback.host_str()
            || url.port_or_known_default() != callback.port_or_known_default()
            || url.path() != callback.path()
        {
            return Err(rejected("Unrecognized authorization callback"));
        }
        let params: HashMap<_, _> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        if params.get("state") != Some(&self.state) {
            return Err(rejected("OAuth state mismatch"));
        }
        if params.contains_key("error") {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Microsoft sign-in cancelled or rejected",
            ));
        }
        let code = params
            .get("code")
            .filter(|c| !c.is_empty())
            .ok_or_else(|| rejected("Missing authorization code"))?;
        Ok(code.clone())
    }
}

pub fn begin(client_id: &str, loopback_port: u16) -> io::Result<AuthRequest> {
    if uuid::Uuid::parse_str(client_id).is_err() || loopback_port == 0 {
        return Err(rejected(
            "A registered Microsoft application ID and loopback port are required",
        ));
    }
    let state = uuid::Uuid::new_v4().to_string();
    let verifier = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let redirect = format!("http://localhost:{loopback_port}/callback");
    let mut url = reqwest::Url::parse(AUTHORIZE_URL).map_err(io::Error::other)?;
    url.query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &redirect)
        .append_pair("response_mode", "query")
        .append_pair("scope", "XboxLive.signin offline_access")
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state);
    Ok(AuthRequest {
        url: url.to_string(),
        redirect,
        state,
        verifier,
    })
}

/// HTTP transport lets authentication be tested without a Microsoft account.
pub trait AuthTransport {
    fn post_form(&self, url: &str, form: &[(&str, &str)]) -> io::Result<Value>;
    fn post_json(&self, url: &str, json: &Value) -> io::Result<Value>;
    fn get_bearer(&self, url: &str, token: &str) -> io::Result<Value>;
}
pub struct AuthHttp {
    client: reqwest::blocking::Client,
}
impl AuthHttp {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            client: reqwest::blocking::Client::new(),
        })
    }
}
fn json_response(
    response: Result<reqwest::blocking::Response, reqwest::Error>,
) -> io::Result<Value> {
    // Do not leak token-bearing request/response bodies through lower-level error messages.
    let res = response.map_err(|_| rejected("Authentication network request failed"))?;
    if !res.status().is_success() {
        return Err(rejected(
            if res.status() == reqwest::StatusCode::FORBIDDEN {
                "Authentication service rejected request (403; Minecraft Services access may require approval)"
            } else {
                "Authentication service rejected request"
            },
        ));
    }
    res.json()
        .map_err(|_| rejected("Invalid authentication service response"))
}
impl AuthTransport for AuthHttp {
    fn post_form(&self, url: &str, form: &[(&str, &str)]) -> io::Result<Value> {
        json_response(self.client.post(url).form(form).send())
    }
    fn post_json(&self, url: &str, body: &Value) -> io::Result<Value> {
        json_response(self.client.post(url).json(body).send())
    }
    fn get_bearer(&self, url: &str, token: &str) -> io::Result<Value> {
        json_response(self.client.get(url).bearer_auth(token).send())
    }
}

/// Access tokens are memory-only. Persist only the refresh token in OS-protected storage.
/// Intentionally not Debug, Clone, Serialize or Display.
pub struct OwnedSession {
    pub name: String,
    pub uuid: String,
    minecraft_token: String,
    refresh_token: Option<String>,
}
impl OwnedSession {
    pub fn into_launch_identity(&self) -> crate::vanilla_launch::Identity {
        crate::vanilla_launch::Identity {
            username: self.name.clone(),
            uuid: self.uuid.clone(),
            minecraft_token: self.minecraft_token.clone(),
            owned_profile: true,
        }
    }
    pub fn refresh_secret(&self) -> Option<&str> {
        self.refresh_token.as_deref()
    }
    pub fn has_refresh_token(&self) -> bool {
        self.refresh_token.is_some()
    }
    pub fn logout(self) {} // dropping the session removes in-memory credentials
}
fn required<'a>(value: &'a Value, key: &str) -> io::Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| rejected("Missing required authentication response field"))
}
fn credentials<T: AuthTransport>(
    transport: &T,
    client_id: &str,
    grant: &[(&str, &str)],
) -> io::Result<(String, Option<String>)> {
    let mut data = vec![
        ("client_id", client_id),
        ("scope", "XboxLive.signin offline_access"),
    ];
    data.extend_from_slice(grant);
    let response = transport.post_form(TOKEN_URL, &data)?;
    let token = required(&response, "access_token")?.to_owned();
    let refresh = response
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok((token, refresh))
}
pub fn exchange_code<T: AuthTransport>(
    transport: &T,
    client_id: &str,
    request: &AuthRequest,
    code: &str,
) -> io::Result<OwnedSession> {
    if uuid::Uuid::parse_str(client_id).is_err() || code.is_empty() {
        return Err(rejected("Invalid OAuth application or code"));
    }
    let (token, refresh) = credentials(
        transport,
        client_id,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", request.redirect_uri()),
            ("code_verifier", &request.verifier),
        ],
    )?;
    finish(transport, &token, refresh)
}
pub fn refresh<T: AuthTransport>(
    transport: &T,
    client_id: &str,
    session: &OwnedSession,
) -> io::Result<OwnedSession> {
    let token = session
        .refresh_secret()
        .ok_or_else(|| rejected("Microsoft sign-in required"))?;
    resume(transport, client_id, token)
}
/// Restore a session from an OS-protected Microsoft refresh credential.
pub fn resume<T: AuthTransport>(
    transport: &T,
    client_id: &str,
    refresh_token: &str,
) -> io::Result<OwnedSession> {
    if uuid::Uuid::parse_str(client_id).is_err() || refresh_token.is_empty() {
        return Err(rejected("Microsoft sign-in required"));
    }
    let (token, rotated) = credentials(
        transport,
        client_id,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ],
    )?;
    finish(
        transport,
        &token,
        rotated.or_else(|| Some(refresh_token.to_owned())),
    )
}

fn finish<T: AuthTransport>(
    transport: &T,
    msa: &str,
    refresh_token: Option<String>,
) -> io::Result<OwnedSession> {
    let xbox=transport.post_json(XBL_URL,&json!({"Properties":{"AuthMethod":"RPS","SiteName":"user.auth.xboxlive.com","RpsTicket":format!("d={msa}")},"RelyingParty":"http://auth.xboxlive.com","TokenType":"JWT"}))?;
    let xbl_token = required(&xbox, "Token")?;
    let xsts=transport.post_json(XSTS_URL,&json!({"Properties":{"SandboxId":"RETAIL","UserTokens":[xbl_token]},"RelyingParty":"rp://api.minecraftservices.com/","TokenType":"JWT"}))?;
    let xsts_token = required(&xsts, "Token")?;
    let uhs = xsts
        .pointer("/DisplayClaims/xui/0/uhs")
        .and_then(Value::as_str)
        .ok_or_else(|| rejected("Missing Xbox user hash"))?;
    let minecraft = transport.post_json(
        MC_LOGIN,
        &json!({"identityToken":format!("XBL3.0 x={uhs};{xsts_token}")}),
    )?;
    let mc_token = required(&minecraft, "access_token")?.to_owned();
    let rights = transport.get_bearer(ENTITLEMENTS, &mc_token)?;
    let owned = rights
        .get("items")
        .and_then(Value::as_array)
        .is_some_and(|a| {
            a.iter().any(|x| {
                x.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| matches!(n, "game_minecraft" | "product_minecraft"))
            })
        });
    if !owned {
        return Err(rejected("Minecraft Java ownership not confirmed"));
    }
    let profile = transport.get_bearer(PROFILE, &mc_token)?;
    let uuid = required(&profile, "id")?.to_string();
    let name = required(&profile, "name")?.to_string();
    if uuid.len() != 32 || !uuid.bytes().all(|x| x.is_ascii_hexdigit()) {
        return Err(rejected("Invalid Minecraft profile ID"));
    }
    Ok(OwnedSession {
        name,
        uuid,
        minecraft_token: mc_token,
        refresh_token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fake {
        entitled: bool,
    }
    impl AuthTransport for Fake {
        fn post_form(&self, _: &str, form: &[(&str, &str)]) -> io::Result<Value> {
            assert!(
                form.iter()
                    .any(|(key, _)| *key == "code_verifier" || *key == "refresh_token")
            );
            Ok(json!({"access_token":"msa", "refresh_token":"refresh"}))
        }
        fn post_json(&self, url: &str, _: &Value) -> io::Result<Value> {
            Ok(match url {
                XBL_URL => json!({"Token":"xbl"}),
                XSTS_URL => json!({"Token":"xsts","DisplayClaims":{"xui":[{"uhs":"hash"}]}}),
                MC_LOGIN => json!({"access_token":"minecraft"}),
                _ => return Err(rejected("Wrong endpoint")),
            })
        }
        fn get_bearer(&self, url: &str, _: &str) -> io::Result<Value> {
            Ok(if url == ENTITLEMENTS {
                json!({"items":if self.entitled {vec![json!({"name":"game_minecraft"})]}else{vec![]}})
            } else if url == PROFILE {
                json!({"id":"12345678123412341234123456789abc","name":"Player"})
            } else {
                return Err(rejected("Wrong endpoint"));
            })
        }
    }
    #[test]
    fn pkce_state_rejected_on_mismatch() {
        let app = "5fe9b6ce-72a4-468d-93ce-991b46667e26";
        let request = begin(app, 32842).unwrap();
        assert!(request.url.contains("code_challenge_method=S256"));
        assert!(
            request
                .accept("http://localhost:32842/callback?code=fake&state=wrong")
                .is_err()
        );
        assert!(begin("", 32842).is_err());
        let url = format!("{}?code=example&state={}", request.redirect, request.state);
        let code = request.accept(&url).unwrap();
        let session = exchange_code(&Fake { entitled: true }, app, &request, &code).unwrap();
        assert_eq!(session.name, "Player");
        assert!(session.has_refresh_token());
        session.logout();
    }
    #[test]
    fn refresh_restores_profile_with_owned_account_only() {
        let app = "5fe9b6ce-72a4-468d-93ce-991b46667e26";
        let restored = resume(&Fake { entitled: true }, app, "saved-refresh").unwrap();
        assert_eq!(restored.uuid, "12345678123412341234123456789abc");
        assert_eq!(restored.refresh_secret(), Some("refresh"));
        assert!(resume(&Fake { entitled: false }, app, "saved-refresh").is_err());
        assert!(resume(&Fake { entitled: true }, app, "").is_err());
    }
    #[test]
    fn entitlement_is_mandatory() {
        assert!(finish(&Fake { entitled: false }, "msa", None).is_err());
    }
}
