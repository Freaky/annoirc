use std::{
    fmt,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{anyhow, Result};
use futures::{channel::oneshot, future::Shared, prelude::*};
use lru_time_cache::LruCache;
use reqwest::header::{HeaderMap, ACCEPT_LANGUAGE, USER_AGENT};
use scraper::{Html, Selector};
use serde::Deserialize;
use slog::{info, o, Logger};
use tokio::time::timeout;
use url::Url;

use crate::{config::*, irc_string::*, twitter::*};

#[derive(Clone, Debug, PartialEq)]
pub struct UrlInfo {
    pub url: Url,
    pub title: IrcString,
    pub desc: Option<IrcString>,
}

#[derive(Clone, Debug, PartialEq, Eq, Ord, PartialOrd, Hash)]
pub enum BotCommand {
    Url(Url),
}

// Consider Boxing these, or moving the Arc internally
#[derive(Clone, Debug)]
pub enum Info {
    Url(UrlInfo),
    Tweet(Tweet),
    Tweeter(Tweeter),
}

#[derive(Debug, Deserialize)]
struct Wiki {
    title: String,
    extract: String,
}

type Response = Shared<oneshot::Receiver<Arc<Result<Info>>>>;

#[derive(Clone)]
pub struct CommandHandler {
    log: Logger,
    config: ConfigMonitor,
    client: reqwest::Client,
    twitter: TwitterHandler,
    cache: Arc<Mutex<LruCache<BotCommand, Response>>>,
}

impl fmt::Display for BotCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Url(url) => write!(f, "Url({})", url),
        }
    }
}

impl std::fmt::Debug for CommandHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandHandler")
            .field("config", &self.config)
            .field("client", &self.client)
            .field("twitter", &self.twitter)
            .field(
                "cache",
                &format!("{} entires", self.cache.lock().unwrap().len()),
            )
            .finish()
    }
}

fn cache_from_config(conf: &Arc<BotConfig>) -> LruCache<BotCommand, Response> {
    LruCache::with_expiry_duration_and_capacity(
        Duration::from_secs(conf.command.cache_time_secs as u64),
        conf.command.cache_entries as usize,
    )
}

impl CommandHandler {
    pub fn new(log: Logger, config: ConfigMonitor) -> Self {
        let conf = config.current();
        let handler = Self {
            log,
            twitter: TwitterHandler::new(config.clone()),
            config,
            client: reqwest::ClientBuilder::new()
                .cookie_store(true)
                .pool_max_idle_per_host(1)
                .build()
                .expect("Couldn't build HTTP client"),
            cache: Arc::new(Mutex::new(cache_from_config(&conf))),
        };

        let cache = handler.cache.clone();
        let mut config = handler.config.clone();
        tokio::spawn(async move {
            while let Some(conf) = config.next().await {
                *cache.lock().unwrap() = cache_from_config(&conf);
            }
        });

        handler
    }

    pub fn spawn(&self, command: BotCommand) -> Response {
        let mut cache = self.cache.lock().unwrap();
        let log = self.log.new(o!("command" => command.to_string()));

        if let Some(res) = cache.get(&command) {
            info!(log, "cached");
            return res.clone();
        }

        info!(log, "execute");

        // Would this be better off just as a JoinHandle?
        let (tx, rx) = oneshot::channel::<Arc<Result<Info>>>();
        let rx = rx.shared();

        cache.insert(command.clone(), rx.clone());

        let handler = self.clone();
        let max_runtime =
            Duration::from_secs(self.config.current().command.max_runtime_secs as u64);

        tokio::spawn(async move {
            let res = match command {
                BotCommand::Url(ref url) => timeout(max_runtime, handler.handle_url(url)).await,
            };

            match res {
                Ok(res) => {
                    info!(log, "complete"; "result" => ?res);
                    tx.send(Arc::new(res))
                }
                Err(_) => {
                    info!(log, "timeout");
                    tx.send(Arc::new(Err(anyhow!("Timed out"))))
                }
            }
        });

        rx
    }

    async fn handle_url(&self, url: &Url) -> Result<Info> {
        if self.config.current().twitter.bearer_token.is_some() {
            if let Some("twitter.com") = url.host_str() {
                if let Some(path) = url.path_segments().map(|c| c.collect::<Vec<_>>()) {
                    if path.len() == 1 || path.len() == 2 && path[1].is_empty() {
                        return self.twitter.fetch_tweeter(path[0]).await.map(Info::Tweeter);
                    } else if path.len() == 3 && path[1] == "status" {
                        if let Ok(id) = path[2].parse::<u64>() {
                            return self.twitter.fetch_tweet(id).await.map(Info::Tweet);
                        }
                    }
                }
            }
        }

        if let Some(domain) = url.host_str() {
            if domain.ends_with(".wikipedia.org") {
                let lang = domain.split('.').next().unwrap();

                if let Some(path) = url.path_segments().map(|c| c.collect::<Vec<_>>()) {
                    if path.len() > 1 && path[0] == "wiki" {
                        let article = path[1];
                        return self.fetch_wikipedia(lang, article).await.map(Info::Url);
                    }
                }
            }
        }

        self.fetch_url(url).await.map(Info::Url)
    }

    fn http_get(&self, url: &Url) -> reqwest::RequestBuilder {
        let config = self.config.current();
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT_LANGUAGE, config.url.accept_language.clone());
        headers.insert(USER_AGENT, config.url.user_agent.clone());

        self.client
            .get(url.clone())
            .timeout(Duration::from_secs(config.url.timeout_secs as u64))
            .headers(headers)
    }

    async fn fetch_wikipedia(&self, lang: &str, article: &str) -> Result<UrlInfo> {
        let url = Url::parse(&format!(
            "https://{}.wikipedia.org/api/rest_v1/page/summary/{}",
            lang, article
        ))?;

        let wiki = self.http_get(&url).send().await?.json::<Wiki>().await?;

        Ok(UrlInfo {
            url,
            title: wiki.title.into(),
            desc: Some(wiki.extract.into()),
        })
    }

    async fn fetch_url(&self, url: &Url) -> Result<UrlInfo> {
        let config = self.config.current();

        let mut res = self.http_get(url).send().await?;

        if !res.status().is_success() {
            return Err(anyhow!("Status {}", res.status()));
        }

        if config.url.globally_routable_only
            && res
                .remote_addr()
                .map(|addr| !ip_rfc::global(&addr.ip()))
                .unwrap_or_default()
        {
            return Err(anyhow!("Restricted IP"));
        }

        if let Some(mime) = res
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|ct| ct.to_str().ok())
            .and_then(|ct| ct.parse::<mime::Mime>().ok())
        {
            if mime.type_() != mime::TEXT {
                return Err(anyhow!("Ignoring mime type {}", mime));
            }
        }

        let byte_limit = config.url.max_kb as usize * 1024;
        let mut chunk_limit = config.url.max_chunks;
        let mut buf = Vec::with_capacity(byte_limit * 2);

        while let Some(chunk) = res.chunk().await? {
            buf.extend(chunk);
            chunk_limit -= 1;

            if buf.len() >= byte_limit || chunk_limit == 0 {
                break;
            }
        }

        let buf = String::from_utf8_lossy(&buf);

        let fragment = Html::parse_document(&buf);
        let title_selector = Selector::parse(r#"title"#).unwrap();
        let description_selector = Selector::parse(r#"meta[name="description"], meta[name="twitter:description"], meta[property="og:description"]"#).unwrap();

        let title = fragment
            .select(&title_selector)
            .next()
            .map(|n| IrcString::from(n.text().collect::<String>()))
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("No title"))?;

        let desc = fragment
            .select(&description_selector)
            .next()
            .and_then(|n| n.value().attr("content"))
            .map(html_escape::decode_html_entities)
            .map(IrcString::from)
            .filter(|s| !s.is_empty());

        Ok(UrlInfo {
            url: res.url().clone(),
            title,
            desc,
        })
    }
}
