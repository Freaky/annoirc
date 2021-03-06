use std::{
    collections::HashMap,
    convert::TryFrom,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{anyhow, Result};
use irc::client::prelude::Config;
use regex::RegexSet;
use reqwest::header::HeaderValue;
use serde::{Deserialize, Deserializer};
use slog::{crit, error, info, warn, Logger};
use tokio::{io::AsyncReadExt, sync::watch};

#[derive(Debug, Clone)]
pub struct ConfigMonitor(watch::Receiver<Arc<BotConfig>>);

#[derive(Debug, Clone)]
pub struct ConfigUpdater(Arc<Mutex<Option<watch::Sender<Arc<BotConfig>>>>>);

#[derive(Default, Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields, default)]
pub struct BotConfig {
    pub command: CommandConfig,
    pub template: TemplateConfig,
    pub url: UrlConfig,
    pub twitter: TwitterConfig,
    pub omdb: OmdbConfig,
    pub youtube: YouTubeConfig,
    pub wolfram: WolframConfig,
    pub defaults: Config,
    pub network: HashMap<String, Config>,
}

#[derive(Default, Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields, default)]
pub struct TwitterConfig {
    pub bearer_token: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields, default)]
pub struct UrlConfig {
    pub max_per_message: u8,
    pub max_kb: u16,
    pub max_chunks: u16,
    pub timeout_secs: u8,
    pub globally_routable_only: bool,
    pub scheme_required: bool,
    pub include_description: bool,
    #[serde(deserialize_with = "parse_header_value")]
    pub user_agent: HeaderValue,
    #[serde(deserialize_with = "parse_header_value")]
    pub accept_language: HeaderValue,
    #[serde(deserialize_with = "parse_regex_set")]
    pub ignore_url_regex: RegexSet,
}

#[derive(Default, Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields, default)]
pub struct OmdbConfig {
    pub api_key: Option<String>,
}

#[derive(Default, Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields, default)]
pub struct YouTubeConfig {
    pub api_key: Option<String>,
    pub lang: Option<String>,
}

#[derive(Default, Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields, default)]
pub struct WolframConfig {
    pub app_id: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields, default)]
pub struct CommandConfig {
    pub max_concurrency: u8,
    pub max_runtime_secs: u8,
    pub cache_time_secs: u32,
    pub cache_entries: u32,
    pub prefix: String,
}

fn parse_header_value<'de, D>(d: D) -> Result<HeaderValue, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(d)?;
    HeaderValue::try_from(s).map_err(serde::de::Error::custom)
}

fn parse_regex_set<'de, D>(d: D) -> Result<RegexSet, D::Error>
where
    D: Deserializer<'de>,
{
    let re = <Vec<String>>::deserialize(d)?;
    RegexSet::new(re).map_err(serde::de::Error::custom)
}

impl Default for UrlConfig {
    fn default() -> Self {
        Self {
            max_per_message: 3,
            timeout_secs: 10,
            max_kb: 256,
            max_chunks: 256,
            globally_routable_only: true,
            scheme_required: false,
            include_description: true,
            user_agent: HeaderValue::from_static(
                "Mozilla/5.0 (FreeBSD 14.0; FreeBSD; x64; rv:81) Gecko/20100101 annoirc/81",
            ),
            accept_language: HeaderValue::from_static("en,*;q=0.5"),
            ignore_url_regex: RegexSet::empty(),
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
            prefix: ".".to_string(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
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
    async fn load(path: &Path) -> Result<BotConfig> {
        const LIMIT: usize = 128 * 1024;
        let mut config = String::new();
        if tokio::fs::File::open(&path)
            .await?
            .take(LIMIT as u64)
            .read_to_string(&mut config)
            .await?
            == LIMIT
        {
            return Err(anyhow!("excessively large configuration"));
        }
        Ok(toml::from_str(&config)?)
    }
}

impl ConfigMonitor {
    /// Begin monitoring the specified configuration file, if it exists
    pub async fn watch<P: Into<PathBuf>>(log: Logger, path: P) -> Result<ConfigMonitor> {
        let path = path.into();

        let config = BotConfig::load(&path).await.map_err(|e| {
            crit!(log, "load"; "status" => "failed", "error" => %e, "path" => %path.display());
            anyhow!("Failed loading initial configuration")
        })?;
        let (tx, rx) = watch::channel(Arc::new(config));

        let tx = ConfigUpdater(Arc::new(Mutex::new(Some(tx))));
        let rx = ConfigMonitor(rx);

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
            use tokio::signal::unix::{signal, SignalKind};

            tokio::spawn(async move {
                let mut term = signal(SignalKind::terminate()).unwrap();
                let mut int = signal(SignalKind::interrupt()).unwrap();
                let mut hup = signal(SignalKind::hangup()).unwrap();

                loop {
                    tokio::select! {
                        Some(_) = term.recv() => {
                            warn!(log, "shutdown"; "signal" => "term");
                            tx.close();
                            break;
                        },
                        Some(_) = int.recv() => {
                            warn!(log, "shutdown"; "signal" => "interrupt");
                            tx.close();
                            break;
                        },
                        Some(_) = hup.recv() => {
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

        Ok(rx)
    }

    /// Retrieve a copy of the current configuration
    pub fn current(&self) -> Arc<BotConfig> {
        self.0.borrow().clone()
    }

    /// Wait for the next configuration update, if any.
    pub async fn next(&mut self) -> Option<Arc<BotConfig>> {
        self.0.changed().await.ok()?;
        Some(self.current())
    }
}

impl ConfigUpdater {
    /// Distribute a new configuration, if possible
    pub fn update(&self, config: BotConfig) -> bool {
        let tx = self.0.lock().unwrap();
        if let Some(tx) = &*tx {
            tx.send(Arc::new(config)).is_ok()
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
