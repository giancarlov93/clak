//! An OAuth sign-in as the token of a connector (or of an API): refreshes the access token
//! shortly before it expires, or after a 401, one request at a time, and saves every
//! refresh (the refresh token may change every round). A rejected refresh isn't retried:
//! a new sign-in by the user is needed.

use crate::BoxFuture;
use crate::mcp_http::{AccessToken, McpError};
use crate::oauth::{self, OAuthGrant};
use clak_wire::json as wire;
use reqwest::Client;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Who saves a refresh: usually the registry, encrypted.
pub type SaveGrant = Box<dyn Fn(&OAuthGrant) + Send + Sync>;

pub struct OAuthToken {
    http: Client,
    /// One refresh at a time: whoever comes later finds the one already done.
    gate: tokio::sync::Mutex<()>,
    grant: Mutex<OAuthGrant>,
    dead: AtomicBool,
    /// Under this lock: retired, and so the refreshes are no longer saved.
    retired: Mutex<bool>,
    saved: SaveGrant,
}

impl OAuthToken {
    pub fn new(http: Client, grant: OAuthGrant, saved: SaveGrant) -> OAuthToken {
        OAuthToken {
            http,
            gate: tokio::sync::Mutex::new(()),
            grant: Mutex::new(grant),
            dead: AtomicBool::new(false),
            retired: Mutex::new(false),
            saved,
        }
    }

    /// The current sign-in, with the latest refresh.
    pub fn grant(&self) -> OAuthGrant {
        self.grant.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn renewable(&self) -> bool {
        let grant = self.grant.lock().unwrap_or_else(|p| p.into_inner());
        grant.refresh_token.is_some() || grant.is_client_flow()
    }

    async fn refresh(&self, stale: Option<&str>) -> Result<bool, McpError> {
        let _turn = self.gate.lock().await;
        let current = self.grant();
        if Some(current.access_token.as_str()) != stale {
            return Ok(true); // another request already refreshed it
        }
        if self.dead.load(Ordering::SeqCst) {
            return Ok(false);
        }
        let renewed = match oauth::refresh(&self.http, &current).await {
            Ok(renewed) => renewed,
            Err(e) if e.rejected => {
                self.dead.store(true, Ordering::SeqCst);
                return Ok(false);
            }
            Err(e) => return Err(McpError::Transport(e.message)),
        };
        *self.grant.lock().unwrap_or_else(|p| p.into_inner()) = renewed.clone();
        let retired = self.retired.lock().unwrap_or_else(|p| p.into_inner());
        if !*retired {
            (self.saved)(&renewed);
        }
        Ok(true)
    }
}

impl AccessToken for OAuthToken {
    fn interactive(&self) -> bool {
        true
    }

    fn current(&self) -> BoxFuture<'_, Result<Option<String>, McpError>> {
        Box::pin(async move {
            let current = self.grant();
            if let Some(at) = current.expires_at
                && at - 60.0 < wire::now()
                && self.renewable()
                && !self.dead.load(Ordering::SeqCst)
            {
                self.refresh(Some(&current.access_token)).await?;
            }
            Ok(Some(self.grant().access_token))
        })
    }

    fn renew(&self, rejected: Option<String>) -> BoxFuture<'_, Result<bool, McpError>> {
        Box::pin(async move { Ok(self.renewable() && self.refresh(rejected.as_deref()).await?) })
    }

    fn dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    fn host(&self) -> Option<String> {
        Some(self.grant().host())
    }

    fn retire(&self) {
        *self.retired.lock().unwrap_or_else(|p| p.into_inner()) = true;
    }
}
