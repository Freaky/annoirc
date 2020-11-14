use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use irc::client::prelude::Config;
use serde::{Deserialize, Serialize};
use slog::{error, info, Logger};
use tokio::sync::watch;

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

    pub async fn watch<P: Into<PathBuf>>(
        log: Logger,
        path: P,
    ) -> Result<watch::Receiver<Arc<BotConfig>>, anyhow::Error> {
        let path = path.into();

        let config = BotConfig::load(&path).await?;
        let (tx, rx) = watch::channel(Arc::new(config));

        let tx = Arc::new(tx);

        let txc = tx.clone();
        tokio::spawn(async move {
            if let Ok(_) = tokio::signal::ctrl_c().await {
                // On termination, set the default configuration which has no networks.
                // Connections should exit gracefully, followed by the main loop.
                let _ = txc.broadcast(Arc::new(BotConfig::default()));
            }
        });

        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};

            tokio::spawn(async move {
                let mut hups = signal(SignalKind::hangup()).unwrap();

                while let Some(_) = hups.recv().await {
                    info!(log, "SIGHUP");

                    match BotConfig::load(&path).await {
                        Ok(c) => {
                            // TODO: compare with existing and only update if different
                            info!(log, "reload");
                            let _ = tx.broadcast(Arc::new(c));
                        }
                        Err(e) => {
                            error!(log, "reload"; "error" => %e);
                        }
                    }
                }
            });
        }

        Ok(rx)
    }
}
