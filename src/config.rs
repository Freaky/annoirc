use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::Error;
use irc::client::prelude::Config;
use reqwest::header::HeaderValue;
use serde::{Deserialize, Serialize};
use slog::{error, info, warn, Logger};
use tokio::sync::watch;

#[derive(Debug, Clone)]
pub struct ConfigMonitor(watch::Receiver<Arc<BotConfig>>);

#[derive(Debug, Clone)]
pub struct ConfigUpdater(Arc<Mutex<Option<watch::Sender<Arc<BotConfig>>>>>);

#[derive(Default, Debug, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields, default)]
pub struct BotConfig {
    pub command: CommandConfig,
    pub template: TemplateConfig,
    pub url: UrlConfig,
    pub twitter: TwitterConfig,
    pub defaults: Config,
    pub network: HashMap<String, Config>,
}

#[derive(Default, Debug, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields, default)]
pub struct TwitterConfig {
    pub bearer_token: Option<String>,
}

#[derive(Serialize, Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields, default)]
pub struct UrlConfig {
    pub max_per_message: u8,
    pub http_timeout_secs: u8,
    pub globally_routable_only: bool,
    pub user_agent: String,
    pub accept_language: String,
}

#[derive(Serialize, Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields, default)]
pub struct CommandConfig {
    pub max_concurrency: u8,
    pub max_runtime_secs: u8,
    pub cache_time_secs: u32,
    pub cache_entries: u32,
}

impl Default for UrlConfig {
    fn default() -> Self {
        Self {
            max_per_message: 3,
            http_timeout_secs: 10,
            globally_routable_only: true,
            user_agent: "Mozilla/5.0 (FreeBSD 14.0; FreeBSD; x64; rv:81) Gecko/20100101 annoirc/81"
                .to_string(),
            accept_language: "en,*;q=0.5".to_string(),
        }
    }
}

impl Default for CommandConfig {
    fn default() -> Self {
        Self {
            max_concurrency: 8,
            max_runtime_secs: 10,
            cache_time_secs: 1800,
            cache_entries: 256,
        }
    }
}

#[derive(Serialize, Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields, default)]
pub struct TemplateConfig {
    pub title: String,
    pub tweet: String,
}

impl Default for TemplateConfig {
    fn default() -> Self {
        Self {
            title: "[{{ host }}] {{ title }}".to_string(),
            tweet: "[Twitter] {{ user.name }}{% if user.verified %}✓{% endif %} (@{{ user.screen_name }}) {{ tweet.text }} | {% if tweet.favorite_count > 0 %}❤️{{ tweet.favorite_count }} {% endif %}{{ tweet.created_at | date(\"%F %H:%M\") }}".to_string()
        }
    }
}

impl BotConfig {
    async fn load(path: &Path) -> Result<BotConfig, anyhow::Error> {
        let config = tokio::fs::read_to_string(&path).await?;
        let config: BotConfig = toml::from_str(&config)?;
        HeaderValue::from_str(&config.url.accept_language)?;
        HeaderValue::from_str(&config.url.user_agent)?;
        Ok(config)
    }
}

impl ConfigMonitor {
    /// Begin monitoring the specified configuration file, if it exists
    pub async fn watch<P: Into<PathBuf>>(log: Logger, path: P) -> Result<ConfigMonitor, Error> {
        let path = path.into();

        let config = BotConfig::load(&path).await?;
        let (tx, mut rx) = watch::channel(Arc::new(config));

        // Discard the initial configuration
        let _ = rx.recv().await;

        let tx = ConfigUpdater(Arc::new(Mutex::new(Some(tx))));

        #[cfg(not(unix))]
        {
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    warn!(log, "shutdown"; "signal" => "interrupt");
                    tx.close();
                }
            });
        }

        #[cfg(unix)]
        {
            use tokio::{
                signal::unix::{signal, SignalKind},
                stream::StreamExt,
            };

            tokio::spawn(async move {
                let mut term = signal(SignalKind::terminate())
                    .unwrap()
                    .map(|_| "TERM")
                    .merge(signal(SignalKind::interrupt()).unwrap().map(|_| "INT"));
                let mut hup = signal(SignalKind::hangup()).unwrap();

                loop {
                    tokio::select! {
                        Some(sig) = term.next() => {
                            warn!(log, "shutdown"; "signal" => sig);
                            tx.close();
                            break;
                        },
                        Some(_) = hup.next() => {
                            match BotConfig::load(&path).await {
                                Ok(c) => {
                                    warn!(log, "reload"; "status" => "updating", "path" => %path.display());
                                    tx.update(c);
                                }
                                Err(e) => {
                                    error!(log, "reload"; "status" => "ignored", "error" => %e, "path" => %path.display());
                                }
                            }
                        },
                        else => {
                            info!(log, "signal"; "status" => "loop exit");
                            break;
                        }
                    }
                }
            });
        }

        Ok(ConfigMonitor(rx))
    }

    /// Retrieve a copy of the current configuration
    pub fn current(&self) -> Arc<BotConfig> {
        self.0.borrow().clone()
    }

    /// Wait for the next configuration update, if any.
    pub async fn next(&mut self) -> Option<Arc<BotConfig>> {
        self.0.recv().await
    }
}

impl ConfigUpdater {
    /// Distribute a new configuration, if possible
    pub fn update(&self, config: BotConfig) -> bool {
        let tx = self.0.lock().unwrap();
        if let Some(tx) = &*tx {
            tx.broadcast(Arc::new(config)).is_ok()
        } else {
            false
        }
    }

    /// Shut down the configuration, ending the `ConfigMonitor` Stream and
    /// preventing future updates.
    pub fn close(&self) -> bool {
        self.0.lock().unwrap().take().is_some()
    }
}
