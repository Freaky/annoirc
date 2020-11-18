use std::{sync::Arc, time::Duration};

use anyhow::Error;
use egg_mode_text::url_entities;
use futures::{stream::FuturesUnordered, TryFutureExt};
use governor::{Quota, RateLimiter};
use irc::client::prelude::*;
use itertools::Itertools;
use nonzero_ext::*;
use slog::{error, info, o, warn, Logger};
use tokio::{stream::StreamExt, task::JoinHandle, time::Instant};
use url::Url;

use crate::{command::*, config::*, irc_string::*, twitter::*};

#[derive(Debug)]
struct CommandResponse {
    log: Logger,
    target: String,
    info: Arc<Result<Info, Error>>,
}

#[derive(Debug)]
pub struct IrcTask {
    name: String,
    log: Logger,
    handler: CommandHandler,
    config: ConfigMonitor,
}

impl IrcTask {
    pub fn spawn(
        log: Logger,
        handler: CommandHandler,
        config: ConfigMonitor,
        name: String,
    ) -> JoinHandle<String> {
        let log = log.new(o!("network" => name.clone()));
        let mut s = Self {
            log,
            handler,
            config,
            name,
        };

        tokio::spawn(async move {
            s.connect_loop().await;
            s.name
        })
    }

    async fn connect_loop(&mut self) {
        let mut conf = self.config.clone();
        let mut next_connection = Instant::now();

        loop {
            let delay = next_connection > Instant::now();

            tokio::select! {
                conn = self.connection(), if !delay => {
                    match conn {
                        Ok(exit) => {
                            info!(self.log, "disconnected");

                            if exit {
                                break;
                            }
                        }
                        Err(e) => {
                            error!(self.log, "disconnected"; "error" => %e);
                        }
                    }
                    let wait = Duration::from_secs(10);
                    info!(self.log, "sleep"; "delay" => ?wait);
                    next_connection = Instant::now() + wait;
                },
                _ = tokio::time::delay_until(next_connection), if delay => { },
                None = conf.next(), if delay => {
                    break;
                }
            }
        }
    }

    async fn connection(&mut self) -> Result<bool, Error> {
        let mut config = self.config.current();

        let netconf = config.network.get(&self.name);
        if netconf.is_none() {
            info!(self.log, "deconfigured");
            return Ok(true);
        }

        let netconf = netconf.unwrap().clone();

        info!(self.log, "connect"; "server" => &netconf.server, "port" => &netconf.port);

        let mut shutdown = false;

        let mut client = Client::from_config(netconf.clone()).await?;
        client.identify()?;

        let mut stream = client.stream()?;
        let mut pending = FuturesUnordered::new();
        let quota = Quota::per_minute(nonzero!(10u32)); // Max of 10 per minute per channel
        let limiter = RateLimiter::keyed(quota);

        loop {
            tokio::select! {
                newconf = self.config.next(), if !shutdown => {
                    // Might be nice to have a timeout set up for dropping the connection.
                    if let Some(newconf) = newconf {
                        config = newconf;
                        if let Some(new_netconf) = config.network.get(&self.name) {
                            if *new_netconf != netconf {
                                info!(self.log, "reconnecting");
                                client.send_quit("Reconnecting")?;
                            }
                        } else {
                            shutdown = true;
                            info!(self.log, "deconfigured");
                            client.send_quit("Disconnecting")?;
                        }
                    } else {
                        shutdown = true;
                        info!(self.log, "disconnecting");
                        client.send_quit("Disconnecting")?;
                    }
                },
                Some(_) = pending.next() => { },
                message = stream.next() => {
                    if message.is_none() {
                        break;
                    }
                    let message = message.unwrap();
                    let message = message?;

                    match &message.command {
                        Command::Response(irc::proto::Response::RPL_ENDOFMOTD, _)
                        | Command::Response(irc::proto::Response::ERR_NOMOTD, _) => {
                            info!(self.log, "connected");
                        },
                        Command::JOIN(ref c, None, None) => {
                            info!(self.log, "join"; "channel" => c);
                        }
                        Command::INVITE(target, channel) if target == client.current_nickname() && netconf.channels.contains(&channel) => {
                            info!(self.log, "invited"; "channel" => channel);
                            // TODO: channel keys
                            client.send_join(channel)?;
                        },
                        Command::KICK(channel, target, reason) if target == client.current_nickname() => {
                            info!(self.log, "kicked"; "channel" => channel, "reason" => reason);
                        },
                        Command::PRIVMSG(target, content) => {
                            if let Some(Prefix::Nickname(nick, _, _)) = &message.prefix {
                                if nick == client.current_nickname() || content.starts_with('\x01') || !netconf.channels.contains(&target) {
                                    continue;
                                }

                                for url in url_entities(&content)
                                    .into_iter()
                                    .filter_map(|url| parse_url(url.substr(content)).ok())
                                    .take(config.url.max_per_message as usize)
                                    .unique()
                                {
                                    if limiter.check_key(&target.clone()).is_err() {
                                        warn!(self.log, "ratelimit"; "channel" => target, "nick" => nick);
                                        break;
                                    }

                                    let cmd = BotCommand::Url(url.clone());
                                    let target = target.clone();
                                    let sender = client.sender();
                                    info!(self.log, "command"; "command" => %cmd);
                                    // Should probably extract this to the future resolution bit
                                    let fut = self.handler.spawn(cmd).map_ok(move |res| {
                                        match *res {
                                            Ok(Info::Url(ref info)) => {
                                                let _ = sender.send_privmsg(
                                                    &target,
                                                    format!(
                                                        "[\x0303\x02\x02{}\x0f] \x0300\x02\x02{}\x0f",
                                                        sanitize(info.url.host_str().unwrap_or(""), 30),
                                                        info.title.trunc(380)
                                                    )
                                                );
                                                if let Some(desc) = &info.desc {
                                                    let _ = sender.send_privmsg(
                                                        &target,
                                                        format!(
                                                            "[\x0303{}\x02\x02\x0f] \x0300\x02\x02{}\x0f",
                                                            sanitize(info.url.host_str().unwrap_or(""), 30),
                                                            desc.trunc(380)
                                                        )
                                                    );
                                                }
                                            },
                                            Ok(Info::Tweet(ref tweet)) => {
                                                let _ = sender.send_privmsg(
                                                    &target,
                                                    format_tweet(tweet)
                                                );
                                                if let Some(quote) = &tweet.quote {
                                                    let _ = sender.send_privmsg(
                                                        &target,
                                                        format_tweet(quote)
                                                    );
                                                }
                                                if let Some(retweet) = &tweet.retweet {
                                                    let _ = sender.send_privmsg(
                                                        &target,
                                                        format_tweet(retweet)
                                                    );
                                                }
                                            },
                                            Ok(Info::Tweeter(ref user)) => {
                                                let _ = sender.send_privmsg(
                                                    &target,
                                                    format_tweeter(user)
                                                );
                                                if let Some(tweet) = &user.status {
                                                    let _ = sender.send_privmsg(
                                                        &target,
                                                        format_tweet(tweet)
                                                    );
                                                }
                                            },
                                            _ => ()
                                        }
                                    });
                                    pending.push(fut);
                                }
                            }
                        },
                        _ => ()
                    }
                },
                else => break
            }
        }

        Ok(shutdown)
    }
}

fn format_tweet(tweet: &Tweet) -> String {
    // not included if retrieved from a user status field
    if let Some(user) = &tweet.user {
        format!(
            "[\x0303Twitter\x0f] \x0304\x02\x02{}\x0f{} (@{}) \x0300\x02\x02{}\x0f | {} {}",
            user.name.trunc(30),
            if user.verified { "✓" } else { "" },
            user.screen_name.trunc(30),
            tweet.text.trunc(300),
            if tweet.favourite_count == 0 {
                "".to_string()
            } else {
                format!("❤️{}", tweet.favourite_count)
            },
            tweet.created_at.format("%F %H:%M")
        )
    } else {
        format!(
            "[\x0303Twitter\x0f] \x0300\x02\x02{}\x0f | {} {}",
            tweet.text.trunc(300),
            if tweet.favourite_count == 0 {
                "".to_string()
            } else {
                format!("❤️{}", tweet.favourite_count)
            },
            tweet.created_at.format("%F %H:%M")
        )
    }
}

fn format_tweeter(user: &Tweeter) -> String {
    format!(
        "[\x0303Twitter\x0f] \x0304\x02\x02{}\x0f{} (@{}) {} Tweets, {} Followers, {}{}",
        user.name.trunc(30),
        if user.verified { "✓" } else { "" },
        user.screen_name.trunc(30),
        user.statuses_count,
        user.followers_count,
        if let Some(ref desc) = user.description {
            format!("\"\x0300\x02\x02{}\x0f\", ", desc.trunc(300))
        } else {
            "".to_string()
        },
        user.created_at.format("%F %H:%M")
    )
}

fn parse_url(text: &str) -> Result<Url, url::ParseError> {
    match Url::parse(text) {
        Ok(url) => Ok(url),
        Err(url::ParseError::RelativeUrlWithoutBase) => Url::parse(&format!("http://{}", text)),
        Err(e) => Err(e),
    }
}
