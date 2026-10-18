//! The whole hub, as the app and `clak-daemon` start it: the core (`clak-hub`) with the
//! real operating system (`clak-platform`), the MCP servers, the REST APIs and the
//! settings as a program.

mod platform;

pub use platform::SystemPlatform;

use clak_apis::ApiHub;
use clak_connectors::{ConnectorHub, ConnectorStore};
use clak_hub::{Broker, Hub, HubOptions, LogSink};
use clak_settings::HubSettings;
use std::path::Path;
use std::sync::Arc;

/// The hub's options with the real system: everything else at the default values.
pub fn options() -> HubOptions {
    HubOptions::new(Arc::new(SystemPlatform))
}

/// The listening hub, with everything that lives in the same process.
pub struct ClakHost {
    hub: Hub,
    connectors: Arc<ConnectorHub>,
    apis: Option<Arc<ApiHub>>,
    settings: Option<Arc<HubSettings>>,
}

impl ClakHost {
    /// Starts the hub on this folder, or nothing if there is one already.
    pub async fn try_start(
        home: &Path,
        options: HubOptions,
        log: Option<LogSink>,
    ) -> std::io::Result<Option<ClakHost>> {
        let timing = options.connector_timing;
        // `Hub::try_start` takes the options: what's needed later is read before.
        let with_settings = options.settings;
        let Some(hub) = Hub::try_start(home, options, log).await? else {
            return Ok(None);
        };
        // The connectors' secrets: DPAPI on Windows, the Keychain on the Mac.
        let secrets = Arc::from(clak_platform::default_box(home, "connectors.secrets"));
        let store = Arc::new(ConnectorStore::new(home, secrets));
        let connectors = ConnectorHub::new(hub.broker().clone(), store, timing);
        connectors.start_all();
        // The REST APIs: without an HTTP client (it shouldn't happen) the hub goes on without them.
        let apis = match ApiHub::start(hub.broker().clone(), home) {
            Ok(apis) => Some(apis),
            Err(e) => {
                hub.broker()
                    .run(|c| c.log(format!("REST APIs unavailable: {e}")));
                None
            }
        };
        // The settings as a program (`clak settings`), after what they change: without the
        // APIs they don't start, because half of their commands would have nowhere to go.
        let settings = match (&apis, with_settings) {
            (Some(apis), true) => {
                let settings =
                    HubSettings::new(hub.broker().clone(), connectors.clone(), apis.clone());
                settings.start();
                Some(settings)
            }
            _ => None,
        };
        Ok(Some(ClakHost {
            hub,
            connectors,
            apis,
            settings,
        }))
    }

    pub fn broker(&self) -> &Broker {
        self.hub.broker()
    }

    pub fn hub(&self) -> &Hub {
        &self.hub
    }

    /// The MCP servers: the panel adds, changes and removes them from here.
    pub fn connectors(&self) -> &Arc<ConnectorHub> {
        &self.connectors
    }

    /// The REST APIs, if they started.
    pub fn apis(&self) -> Option<&Arc<ApiHub>> {
        self.apis.as_ref()
    }

    /// The settings as a program, if they're on ([`HubOptions::settings`]).
    pub fn settings(&self) -> Option<&Arc<HubSettings>> {
        self.settings.as_ref()
    }

    /// Stops listening, stops the settings, the connectors and the APIs, removes the lockfile.
    pub async fn stop(self) {
        if let Some(settings) = &self.settings {
            settings.stop("hub closed").await;
        }
        self.connectors.stop("hub closed").await;
        if let Some(apis) = &self.apis {
            apis.stop().await;
        }
        self.hub.stop().await;
    }
}
