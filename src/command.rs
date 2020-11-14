use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Error};
use egg_mode::tweet;
use futures::channel::oneshot;
use futures::future::Shared;
use futures::prelude::*;
use scraper::{Html, Selector};
use url::Url;

use crate::config::*;
#[derive(Clone, Debug, PartialEq)]
pub struct UrlInfo {
    pub url: Url,
    pub title: String,
    pub desc: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum BotCommand {
    Url(Url),
}

#[derive(Clone, Debug)]
pub enum Info {
    Url(UrlInfo), // A final Url after redirects and a title string
    Tweet(tweet::Tweet),
    Tweeter(egg_mode::user::TwitterUser),
}

#[derive(Clone, Debug)]
pub struct Response {
    pub info: Shared<oneshot::Receiver<Arc<Result<Info, Error>>>>,
    pub ts: Instant,
}

pub static USER_AGENT: &str = concat!("Mozilla/5.0 annobot", "/", env!("CARGO_PKG_VERSION"),);

#[derive(Clone, Debug)]
pub struct CommandHandler {
    config: ConfigMonitor,
    client: reqwest::Client,
    cache: Arc<Mutex<HashMap<BotCommand, Response>>>,
}

impl CommandHandler {
    pub fn new(config: ConfigMonitor) -> Self {
        Self {
            config,
            client: reqwest::ClientBuilder::new()
                .cookie_store(true)
                .user_agent(USER_AGENT)
                .timeout(Duration::from_secs(10))
                .pool_max_idle_per_host(1)
                .build()
                .expect("Couldn't build HTTP client"),
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn spawn(
        &self,
        command: BotCommand,
    ) -> Shared<oneshot::Receiver<Arc<Result<Info, Error>>>> {
        let mut cache = self.cache.lock().unwrap();

        // TODO: run this periodically and make the cache configurable
        let now = Instant::now();
        let oldest = now - Duration::from_secs(60 * 3600);
        cache.retain(|_, resp| resp.ts > oldest);

        if let Some(res) = cache.get(&command) {
            return res.info.clone();
        }

        // Would this be better off just as a JoinHandle?
        let (tx, rx) = oneshot::channel::<Arc<Result<Info, anyhow::Error>>>();
        let rx = rx.shared();

        cache.insert(
            command.clone(),
            Response {
                info: rx.clone(),
                ts: now,
            },
        );

        let client = self.client.clone();
        let config = self.config.current();

        tokio::spawn(async move {
            let res = match command {
                BotCommand::Url(url) => handle_url(client, config, url).await,
            };

            tx.send(Arc::new(res))
        });

        rx
    }
}

async fn handle_url(
    client: reqwest::Client,
    config: Arc<BotConfig>,
    url: Url,
) -> Result<Info, Error> {
    if let Some(token) = &config.twitter.bearer_token {
        if let Some("twitter.com") = url.host_str() {
            let token = egg_mode::auth::Token::Bearer(token.to_string());
            if let Some(path) = url.path_segments().map(|c| c.collect::<Vec<_>>()) {
                if path.len() == 1 {
                    return fetch_tweeter(token, &path[0]).await.map(Info::Tweeter);
                } else if path.len() == 3 && path[1] == "status" {
                    if let Ok(id) = path[2].parse::<u64>() {
                        return fetch_tweet(token, id).await.map(Info::Tweet);
                    }
                }
            }
        }
    }

    fetch_url(client, url).await.map(Info::Url)
}

// TODO: Rate limit handling
async fn fetch_tweet(token: egg_mode::auth::Token, id: u64) -> Result<tweet::Tweet, Error> {
    Ok(egg_mode::tweet::show(id, &token).await?.response)
}

async fn fetch_tweeter(
    token: egg_mode::auth::Token,
    id: &str,
) -> Result<egg_mode::user::TwitterUser, Error> {
    Ok(egg_mode::user::show(id.to_string(), &token).await?.response)
}

async fn fetch_url(client: reqwest::Client, url: Url) -> Result<UrlInfo, Error> {
    let mut res = client.get(url).send().await?;

    if res
        .remote_addr()
        .map(|addr| !ip_rfc::global(&addr.ip()))
        .unwrap_or_default()
    {
        return Err(anyhow!("Restricted IP"));
    }

    let limit = 256 * 1024;
    let mut buf = Vec::with_capacity(limit);

    // TODO: skip if an obviously uninteresting MIME type
    while let Some(chunk) = res.chunk().await? {
        buf.extend(chunk);
        if buf.len() >= limit {
            break;
        }
    }

    let buf = String::from_utf8_lossy(&buf);

    let fragment = Html::parse_document(&buf);
    let title_selector = Selector::parse("title").unwrap();
    let description_selector = Selector::parse(r#"meta[name="description"]"#).unwrap();

    let title = fragment
        .select(&title_selector)
        .next()
        .map(|n| n.text().collect::<String>())
        .ok_or_else(|| anyhow!("No title"))?;

    let desc = fragment
        .select(&description_selector)
        .next()
        .and_then(|n| n.value().attr("content"))
        .map(html_escape::decode_html_entities);

    Ok(UrlInfo {
        url: res.url().clone(),
        title,
        desc: desc.map(|s| s.to_string()),
    })
}
