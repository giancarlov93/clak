//! The OAuth sign-in to MCP servers: the fake server is also its sign-in service, and the
//! "browser" follows the sign-in page with an HTTP request, as a user who accepts
//! would.

mod fake;

use clak_connectors::oauth::{self, OAuthClient, OAuthGrant, OpenBrowser};
use clak_connectors::{
    AccessToken, ConnectorStore, McpError, McpHttp, OAuthToken, connector, new_http_client, probe,
};
use fake::FakeMcp;
use serde_json::{Map, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use url::Url;

const TIMEOUT: Duration = Duration::from_secs(10);

async fn server() -> FakeMcp {
    let server = FakeMcp::start().await;
    server.add_fn("ora", "Che ore sono.", |_| "mezzogiorno".into());
    server.with(|s| s.oauth = true);
    server
}

/// The user's browser: opens the sign-in page and follows the redirect to the return page
/// on 127.0.0.1. With `forge`, first a page arrives there with a state that isn't the
/// expected one (another site trying to slip in its own code): its outcome goes in `forged`.
fn browser_with(forge: bool, forged: Arc<Mutex<Vec<u16>>>) -> OpenBrowser {
    Arc::new(move |page: Url| {
        let forged = forged.clone();
        tokio::spawn(async move {
            let http = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap();
            let answer = http.get(page).send().await.unwrap();
            let back = answer.headers()[reqwest::header::LOCATION]
                .to_str()
                .unwrap()
                .to_owned();
            if forge {
                let base = back.split('?').next().unwrap();
                let wrong = http
                    .get(format!("{base}?code=rubato&state=altro"))
                    .send()
                    .await
                    .unwrap();
                forged.lock().unwrap().push(wrong.status().as_u16());
            }
            let done = http.get(&back).send().await.unwrap();
            assert!(done.text().await.unwrap().contains("Clak"));
        });
        Box::pin(async { Ok(()) })
    })
}

fn browser() -> OpenBrowser {
    browser_with(false, Arc::default())
}

async fn sign_in(
    server: &FakeMcp,
    client: Option<&OAuthClient>,
    previous: Option<&OAuthGrant>,
) -> Result<Option<OAuthGrant>, oauth::OAuthError> {
    connector::sign_in(
        &new_http_client(),
        &server.url,
        client,
        previous,
        &browser(),
        None,
        TIMEOUT,
    )
    .await
}

fn mcp(server: &FakeMcp, token: Arc<dyn AccessToken>) -> McpHttp {
    McpHttp::new(
        new_http_client(),
        Url::parse(&server.url).unwrap(),
        Some(token),
        true,
    )
}

fn text(answer: &Map<String, Value>) -> String {
    answer["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_owned()
}

fn temporary_home() -> PathBuf {
    std::env::temp_dir().join(format!("clak-oauth-{}", clak_wire::json::random_hex(6)))
}

/// A connector with OAuth as the hub will set it up: the token refreshes itself and every refresh goes to the registry.
fn oauth_token(store: &Arc<ConnectorStore>, id: &str, grant: OAuthGrant) -> Arc<OAuthToken> {
    let (store, id) = (store.clone(), id.to_owned());
    Arc::new(OAuthToken::new(
        new_http_client(),
        grant,
        Box::new(move |renewed| store.set_oauth(&id, renewed).unwrap()),
    ))
}

#[tokio::test]
async fn un_server_con_oauth_si_aggiunge_con_l_accesso_nel_browser() {
    let server = server().await;
    let found = probe(&server.url, None).await;
    assert!(!found.ok);
    assert!(found.sign_in);
    assert_eq!(
        found.message,
        "The server asks you to sign in, at 127.0.0.1 (OAuth)."
    );

    let steps: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = steps.clone();
    let status = move |s: &str| seen.lock().unwrap().push(s.to_owned());
    let grant = connector::sign_in(
        &new_http_client(),
        &server.url,
        None,
        None,
        &browser(),
        Some(&status),
        TIMEOUT,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(server.registrations(), 1);
    assert!(grant.registered);
    assert_eq!(
        server.with(|s| s.last_authorize["resource"].clone()),
        server.url
    );
    assert_eq!(server.with(|s| s.last_authorize["scope"].clone()), "read"); // the one from the challenge, not all the possible ones
    let steps = steps.lock().unwrap().clone();
    assert!(
        steps
            .iter()
            .any(|s| s.starts_with("Waiting for you to sign in at 127.0.0.1")),
        "{steps:?}"
    );
    assert_eq!(
        steps.first().map(String::as_str),
        Some("Contacting 127.0.0.1…".to_string().as_str())
    );
    assert_eq!(grant.scope.as_deref(), Some("read write"));
    assert!(grant.expires_at.is_some());

    let home = temporary_home();
    let store = Arc::new(ConnectorStore::new(&home, fake::test_box(&home)));
    let entry = store.add_oauth("Finto", &server.url, &grant, None).unwrap();
    assert!(store.get(&entry.id).unwrap().is_oauth());
    assert_eq!(store.oauth(&entry.id), Some(grant.clone()));
    let token = oauth_token(&store, &entry.id, grant.clone());
    assert_eq!(token.host().as_deref(), Some("127.0.0.1"));
    let client = mcp(&server, token);
    client.initialize().await.unwrap();
    assert_eq!(
        text(&client.call_tool("ora", &Map::new(), None).await.unwrap()),
        "mezzogiorno"
    );
    assert!(server.with(|s| {
        s.authorizations
            .contains(&format!("Bearer {}", grant.access_token))
    }));

    // Neither the access token nor the refresh token leaves here in the clear: not on disk, not printed.
    let disk: String = std::fs::read_dir(&home)
        .unwrap()
        .map(|f| std::fs::read_to_string(f.unwrap().path()).unwrap())
        .collect();
    for secret in [&grant.access_token, grant.refresh_token.as_ref().unwrap()] {
        assert!(!disk.contains(secret.as_str()));
    }
    assert!(!format!("{grant:?}").contains(&grant.access_token));
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn un_access_token_scaduto_si_rinnova_da_solo_e_il_rinnovo_si_salva() {
    let server = server().await;
    let grant = sign_in(&server, None, None).await.unwrap().unwrap();
    let home = temporary_home();
    let store = Arc::new(ConnectorStore::new(&home, fake::test_box(&home)));
    let entry = store.add_oauth("Finto", &server.url, &grant, None).unwrap();
    let client = mcp(&server, oauth_token(&store, &entry.id, grant.clone()));
    client.initialize().await.unwrap();
    assert_eq!(
        text(&client.call_tool("ora", &Map::new(), None).await.unwrap()),
        "mezzogiorno"
    );

    server.expire_access_tokens();
    assert_eq!(
        text(&client.call_tool("ora", &Map::new(), None).await.unwrap()),
        "mezzogiorno"
    );
    assert_eq!(server.with(|s| s.refreshes), 1);
    let stored = ConnectorStore::new(&home, fake::test_box(&home))
        .oauth(&entry.id)
        .unwrap();
    assert_ne!(grant.refresh_token, stored.refresh_token); // the service rotated it, and the new one is saved
    assert_ne!(grant.access_token, stored.access_token);
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn un_access_token_che_sta_per_scadere_si_rinnova_prima() {
    let server = server().await;
    server.with(|s| s.access_lifetime = 30); // less than the one-minute margin
    let grant = sign_in(&server, None, None).await.unwrap().unwrap();
    let saved: Arc<Mutex<Vec<OAuthGrant>>> = Arc::default();
    let keep = saved.clone();
    let token = Arc::new(OAuthToken::new(
        new_http_client(),
        grant,
        Box::new(move |g| keep.lock().unwrap().push(g.clone())),
    ));
    mcp(&server, token).initialize().await.unwrap();
    assert!(server.with(|s| s.refreshes) > 0);
    assert!(!saved.lock().unwrap().is_empty());
}

#[tokio::test]
async fn un_accesso_ritirato_non_salva_piu_i_suoi_rinnovi() {
    let server = server().await;
    let grant = sign_in(&server, None, None).await.unwrap().unwrap();
    let saved: Arc<Mutex<usize>> = Arc::default();
    let keep = saved.clone();
    let token = Arc::new(OAuthToken::new(
        new_http_client(),
        grant,
        Box::new(move |_| *keep.lock().unwrap() += 1),
    ));
    token.retire();
    server.expire_access_tokens();
    mcp(&server, token.clone()).initialize().await.unwrap();
    assert_eq!(server.with(|s| s.refreshes), 1);
    assert_eq!(*saved.lock().unwrap(), 0);
}

#[tokio::test]
async fn un_accesso_revocato_si_rifa_con_lo_stesso_client() {
    let server = server().await;
    let grant = sign_in(&server, None, None).await.unwrap().unwrap();
    let token = Arc::new(OAuthToken::new(
        new_http_client(),
        grant.clone(),
        Box::new(|_| {}),
    ));
    let client = mcp(&server, token.clone());
    client.initialize().await.unwrap();

    server.revoke_everything();
    let error = client
        .call_tool("ora", &Map::new(), None)
        .await
        .unwrap_err();
    let McpError::Auth {
        message,
        sign_in: true,
        ..
    } = &error
    else {
        panic!("{error:?}")
    };
    assert!(message.contains("sign-in expired or revoked"), "{message}");
    assert!(token.dead());
    let requests = server.requests();
    assert!(client.call_tool("ora", &Map::new(), None).await.is_err());
    assert_eq!(server.requests(), requests + 1); // a dead sign-in no longer refreshes: one request, no refresh

    let again = sign_in(&server, None, Some(&grant)).await.unwrap().unwrap();
    assert_eq!(again.client_id, grant.client_id); // the same client, no new registration
    assert_eq!(again.redirect_uri, grant.redirect_uri); // and the same port, if it's free
    assert_eq!(server.registrations(), 1);
    let client = mcp(
        &server,
        Arc::new(OAuthToken::new(new_http_client(), again, Box::new(|_| {}))),
    );
    client.initialize().await.unwrap();
    assert_eq!(
        text(&client.call_tool("ora", &Map::new(), None).await.unwrap()),
        "mezzogiorno"
    );
}

#[tokio::test]
async fn senza_accesso_un_server_oauth_dice_dove_accedere() {
    let server = server().await;
    let client = McpHttp::new(
        new_http_client(),
        Url::parse(&server.url).unwrap(),
        None,
        false,
    );
    let error = client.initialize().await.unwrap_err();
    let McpError::Auth {
        challenge: Some(challenge),
        sign_in: false,
        ..
    } = &error
    else {
        panic!("{error:?}")
    };
    assert_eq!(
        challenge.resource_metadata.as_deref(),
        Some(format!("{}/.well-known/oauth-protected-resource/mcp", server.origin).as_str())
    );
    assert_eq!(challenge.scope.as_deref(), Some("read"));
}

#[tokio::test]
async fn un_no_nel_browser_non_da_niente() {
    let server = server().await;
    server.with(|s| s.deny_sign_in = true);
    let error = sign_in(&server, None, None).await.unwrap_err();
    assert_eq!(
        error.message,
        "Sign-in was refused or cancelled in the browser."
    );
}

#[tokio::test]
async fn una_risposta_che_dice_di_venire_da_un_altro_servizio_si_rifiuta() {
    let server = server().await;
    server.with(|s| s.wrong_issuer = true);
    let error = sign_in(&server, None, None).await.unwrap_err();
    assert_eq!(
        error.message,
        "The sign-in answer doesn't come from the expected service: refused."
    );
}

#[tokio::test]
async fn la_pagina_di_ritorno_ignora_chi_non_ha_lo_state_giusto() {
    let server = server().await;
    let forged: Arc<Mutex<Vec<u16>>> = Arc::default();
    let grant = connector::sign_in(
        &new_http_client(),
        &server.url,
        None,
        None,
        &browser_with(true, forged.clone()),
        None,
        TIMEOUT,
    )
    .await
    .unwrap()
    .unwrap();
    assert_ne!(grant.access_token, "rubato");
    assert_eq!(*forged.lock().unwrap(), [400]);
}

#[tokio::test]
async fn senza_un_accesso_nel_tempo_previsto_si_smette_di_aspettare() {
    let server = server().await;
    let nobody: OpenBrowser = Arc::new(|_| Box::pin(async { Ok(()) }));
    let error = connector::sign_in(
        &new_http_client(),
        &server.url,
        None,
        None,
        &nobody,
        None,
        Duration::from_millis(300),
    )
    .await
    .unwrap_err();
    assert_eq!(error.message, "No sign-in within 0 minutes: try again.");
}

#[tokio::test]
async fn senza_registrazione_dinamica_serve_il_client_dell_utente() {
    let server = server().await;
    server.with(|s| s.allow_registration = false);
    let error = sign_in(&server, None, None).await.unwrap_err();
    assert!(
        error
            .message
            .contains("enter its client ID under “OAuth client”"),
        "{}",
        error.message
    );
    assert!(
        error
            .message
            .starts_with("127.0.0.1 doesn't let apps register by themselves")
    );

    server.add_manual_client();
    let grant = sign_in(
        &server,
        Some(&OAuthClient::new("manual-client", None)),
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(grant.client_id, "manual-client");
    assert!(!grant.registered);
    assert_eq!(grant.client_auth, "none");
    assert_eq!(server.registrations(), 0);
}

#[tokio::test]
async fn togliere_il_connettore_revoca_l_accesso() {
    let server = server().await;
    let grant = sign_in(&server, None, None).await.unwrap().unwrap();
    let home = temporary_home();
    let store = ConnectorStore::new(&home, fake::test_box(&home));
    let entry = store.add_oauth("Finto", &server.url, &grant, None).unwrap();
    let refresh = store.oauth(&entry.id).unwrap().refresh_token.unwrap();

    let removed = store.oauth(&entry.id).unwrap();
    store.remove(&entry.id).unwrap();
    oauth::revoke(&new_http_client(), &removed).await;
    assert!(server.with(|s| s.revoked.contains(&refresh)));
    assert_eq!(
        ConnectorStore::new(&home, fake::test_box(&home)).oauth(&entry.id),
        None
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn passare_a_un_token_dimentica_l_accesso_e_viceversa() {
    let server = server().await;
    let grant = sign_in(&server, None, None).await.unwrap().unwrap();
    let home = temporary_home();
    let store = ConnectorStore::new(&home, fake::test_box(&home));
    let entry = store.add_oauth("Finto", &server.url, &grant, None).unwrap();

    store.set_token(&entry.id, Some("incollato")).unwrap();
    let reread = ConnectorStore::new(&home, fake::test_box(&home));
    assert_eq!(reread.oauth(&entry.id), None);
    assert_eq!(reread.token(&entry.id).as_deref(), Some("incollato"));
    assert!(!reread.get(&entry.id).unwrap().is_oauth());

    // The server wants its own sign-in, and a pasted token isn't its own.
    let pasted = McpHttp::new(
        new_http_client(),
        Url::parse(&server.url).unwrap(),
        Some(Arc::new(clak_connectors::StaticToken::new("incollato"))),
        false,
    );
    assert!(matches!(
        pasted.initialize().await,
        Err(McpError::Auth { sign_in: false, .. })
    ));

    store.set_oauth(&entry.id, &grant).unwrap();
    assert_eq!(store.token(&entry.id), None);
    assert!(store.get(&entry.id).unwrap().is_oauth());
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn un_server_aperto_non_chiede_nessun_accesso() {
    let server = server().await;
    server.with(|s| s.oauth = false);
    assert_eq!(sign_in(&server, None, None).await.unwrap(), None);
    assert_eq!(server.registrations(), 0);
}

#[tokio::test]
async fn un_server_senza_metadati_non_permette_l_accesso() {
    let server = server().await;
    server.with(|s| s.oauth = false);
    let error = oauth::discover(&new_http_client(), &Url::parse(&server.url).unwrap(), None)
        .await
        .unwrap_err();
    assert_eq!(
        error.message,
        "This server doesn't support signing in (OAuth): use a token."
    );
    let found = connector::sign_in(
        &new_http_client(),
        "http://127.0.0.1:1/mcp",
        None,
        None,
        &browser(),
        None,
        TIMEOUT,
    )
    .await;
    assert!(
        found
            .unwrap_err()
            .message
            .starts_with("127.0.0.1 not reached: ")
    );
}

// =========================================================================
// The variants for REST APIs
// =========================================================================

#[tokio::test]
async fn un_api_con_indirizzi_noti_fa_l_accesso_senza_scoperta_e_senza_resource() {
    let server = server().await;
    server.with(|s| s.require_resource = false);
    server.add_manual_client();
    let origin = &server.origin;
    let found = oauth::known(
        &format!("{origin}/authorize"),
        &format!("{origin}/token"),
        Some("read"),
    )
    .unwrap();
    let grant = oauth::authorize(
        &new_http_client(),
        &found,
        Some(&OAuthClient::new("manual-client", Some("s3".into()))),
        None,
        &browser(),
        None,
        TIMEOUT,
    )
    .await
    .unwrap();
    assert_eq!(grant.resource, "");
    assert_eq!(grant.client_auth, "client_secret_post");
    assert!(!server.with(|s| s.last_authorize.contains_key("resource")));
    assert_eq!(server.with(|s| s.last_authorize["scope"].clone()), "read");
    let form = server.with(|s| s.token_forms.last().cloned()).unwrap();
    assert_eq!(form.get("client_secret").map(String::as_str), Some("s3"));
    assert!(!form.contains_key("resource"));

    // The refresh, also without resource.
    let renewed = oauth::refresh(&new_http_client(), &grant).await.unwrap();
    assert_ne!(renewed.access_token, grant.access_token);
    assert!(!server.with(|s| s.token_forms.last().unwrap().contains_key("resource")));
}

#[tokio::test]
async fn l_accesso_solo_app_chiede_un_token_e_si_rinnova_rifacendolo() {
    let server = server().await;
    let origin = &server.origin;
    let found = oauth::known(&format!("{origin}/token"), &format!("{origin}/token"), None).unwrap();
    let http = new_http_client();

    let missing = oauth::client_credentials(&http, &found, &OAuthClient::new("app-client", None))
        .await
        .unwrap_err();
    assert_eq!(
        missing.message,
        "The app-only sign-in needs the client secret."
    );
    let wrong = oauth::client_credentials(
        &http,
        &found,
        &OAuthClient::new("app-client", Some("no".into())),
    )
    .await
    .unwrap_err();
    assert!(wrong.rejected);
    assert_eq!(wrong.message, "the sign-in service refused: invalid_client");

    let grant = oauth::client_credentials(
        &http,
        &found,
        &OAuthClient::new("app-client", Some("app-secret".into())),
    )
    .await
    .unwrap();
    assert!(grant.is_client_flow());
    assert_eq!(grant.refresh_token, None);
    let token = Arc::new(OAuthToken::new(
        http.clone(),
        grant.clone(),
        Box::new(|_| {}),
    ));
    let client = mcp(&server, token.clone());
    client.initialize().await.unwrap();

    // Expired: a new token with the same secret, no user.
    server.expire_access_tokens();
    assert_eq!(
        text(&client.call_tool("ora", &Map::new(), None).await.unwrap()),
        "mezzogiorno"
    );
    assert!(
        server.with(|s| s
            .token_forms
            .iter()
            .filter(|f| f["grant_type"] == "client_credentials")
            .count())
            >= 2
    );
    assert!(!token.dead());
}
