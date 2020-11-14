use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Error;
use irc::client::prelude::Config;
use serde::{Deserialize, Serialize};
use slog::{error, info, Logger};
use tokio::sync::watch;

#[derive(Debug, Clone)]
pub struct ConfigMonitor(watch::Receiver<Arc<BotConfig>>);

#[derive(Debug, Clone)]
pub struct ConfigUpdater(Arc<Mutex<Option<watch::Sender<Arc<BotConfig>>>>>);

#[derive(Default, Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct BotConfig {
    pub template: TemplateConfig,
    pub http: HttpConfig,
    pub twitter: TwitterConfig,
    pub defaults: Config,
    pub network: HashMap<String, Config>,
}

#[derive(Default, Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct TwitterConfig {
    pub bearer_token: Option<String>,
}

#[derive(Serialize, Debug, Deserialize, Clone)]
#[serde(default)]
pub struct HttpConfig {
    pub max_per_message: u8,
    pub network_workers: u8,
    pub network_timeout_secs: u16,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            max_per_message: 3,
            network_workers: 4,
            network_timeout_secs: 10,
        }
    }
}

#[derive(Serialize, Debug, Deserialize, Clone)]
#[serde(default)]
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
        let txx = tx.clone();
        let logx = log.clone();
        tokio::spawn(async move {
            if let Ok(_) = tokio::signal::ctrl_c().await {
                info!(logx, "INTERRUPT");
                txx.close();
            }
        });

        #[cfg(unix)]
        {
            // TODO: merge all these into a task with a select loop
            use tokio::signal::unix::{signal, SignalKind};

            let logx = log.clone();
            let txx = tx.clone();
            tokio::spawn(async move {
                if let Some(_) = signal(SignalKind::terminate()).unwrap().recv().await {
                    info!(logx, "SIGTERM");
                    txx.close();
                }
            });

            tokio::spawn(async move {
                let mut hups = signal(SignalKind::hangup()).unwrap();

                while let Some(_) = hups.recv().await {
                    info!(log, "SIGHUP");

                    match BotConfig::load(&path).await {
                        Ok(c) => {
                            // TODO: compare with existing and only update if different
                            info!(log, "reload");
                            tx.update(c);
                        }
                        Err(e) => {
                            error!(log, "reload"; "error" => %e);
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
