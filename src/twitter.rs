use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Error};
use egg_mode::{tweet, user, RateLimit};

use crate::{config::*, irc_string::*};

#[derive(Debug, Clone)]
pub struct TwitterHandler {
    config: ConfigMonitor,
    limiter: Arc<Mutex<Option<RateLimit>>>,
}

#[derive(Debug, Clone)]
pub struct Tweet {
    pub id: u64,
    pub created_at: chrono::DateTime<chrono::offset::Utc>,
    pub favourite_count: i32,
    pub quote: Option<Box<Tweet>>,
    pub retweet: Option<Box<Tweet>>,
    pub text: IrcString,
    pub user: Option<Box<Tweeter>>,
}

#[derive(Debug, Clone)]
pub struct Tweeter {
    pub id: u64,
    pub created_at: chrono::DateTime<chrono::offset::Utc>,
    pub name: IrcString,
    pub screen_name: IrcString,
    pub url: Option<IrcString>,
    pub verified: bool,
    pub description: Option<IrcString>,
    pub statuses_count: i32,
    pub followers_count: i32,
    pub friends_count: i32,
    pub location: Option<IrcString>,
    pub status: Option<Box<Tweet>>,
}

impl From<tweet::Tweet> for Tweet {
    fn from(tweet: tweet::Tweet) -> Self {
        Self {
            id: tweet.id,
            created_at: tweet.created_at,
            favourite_count: tweet.favorite_count,
            quote: tweet.quoted_status.map(|s| s.into()),
            retweet: tweet.retweeted_status.map(|s| s.into()),
            text: tweet.text.into(),
            user: tweet.user.map(|u| u.into()),
        }
    }
}

impl From<Box<tweet::Tweet>> for Box<Tweet> {
    fn from(tweet: Box<tweet::Tweet>) -> Self {
        Box::new(Tweet::from(*tweet))
    }
}

impl From<user::TwitterUser> for Tweeter {
    fn from(user: user::TwitterUser) -> Self {
        Self {
            id: user.id,
            created_at: user.created_at,
            name: user.name.into(),
            screen_name: user.screen_name.into(),
            url: user.url.map(|s| s.into()),
            verified: user.verified,
            description: user.description.map(|s| s.into()),
            statuses_count: user.statuses_count,
            followers_count: user.followers_count,
            friends_count: user.friends_count,
            location: user.location.map(|s| s.into()),
            status: user.status.map(|s| s.into()),
        }
    }
}

impl From<Box<user::TwitterUser>> for Box<Tweeter> {
    fn from(tweeter: Box<user::TwitterUser>) -> Self {
        Box::new(Tweeter::from(*tweeter))
    }
}

impl TwitterHandler {
    pub fn new(config: ConfigMonitor) -> Self {
        Self {
            config,
            limiter: Default::default(),
        }
    }

    // TODO: Rate limit handling is a bit racy
    // Replace t.com redirections with original URLs via UrlEntities if they're not too long
    pub async fn fetch_tweet(&self, id: u64) -> Result<Tweet, Error> {
        let token = self.get_token()?;

        let resp = egg_mode::tweet::show(id, &token).await?;
        self.limiter.lock().unwrap().replace(resp.rate_limit_status);
        Ok(resp.response.into())
    }

    pub async fn fetch_tweeter(&self, id: &str) -> Result<Tweeter, Error> {
        let token = self.get_token()?;

        let resp = egg_mode::user::show(id.to_string(), &token).await?;
        self.limiter.lock().unwrap().replace(resp.rate_limit_status);
        Ok(resp.response.into())
    }

    fn get_token(&self) -> Result<egg_mode::auth::Token, Error> {
        use std::time::SystemTime;

        if self
            .limiter
            .lock()
            .unwrap()
            .filter(|limit| {
                limit.remaining == 0
                    && limit.reset as u64
                        > SystemTime::now()
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs()
            })
            .is_some()
        {
            return Err(anyhow!("Rate limited"));
        }

        self.config
            .current()
            .twitter
            .bearer_token
            .clone()
            .map(egg_mode::auth::Token::Bearer)
            .ok_or_else(|| anyhow!("Not configured"))
    }
}
