use std::time::Duration;

use anyhow::{anyhow, Error};
use egg_mode_text::url_entities;
use futures::stream::FuturesUnordered;
use futures::TryFutureExt;
use governor::{Quota, RateLimiter};
use irc::client::prelude::*;
use itertools::Itertools;
use nonzero_ext::*;
use slog::{error, info, o, warn, Logger};
use tokio::stream::StreamExt;
use url::Url;

use crate::{command::*, config::*, irc_string::*, twitter::*};

pub async fn irc_instance(
    log: Logger,
    handler: CommandHandler,
    name: String,
    config_update: ConfigMonitor,
) {
    loop {
        match irc_connect(log.clone(), handler.clone(), &name, config_update.clone()).await {
            Ok(exit) => {
                info!(log, "disconnect");
                if exit {
                    break;
                }
            }
            Err(e) => {
                error!(log, "disconnect"; "error" => e.to_string());
            }
        }

        // TODO: backoff, log wait time
        tokio::time::delay_for(Duration::from_secs(10)).await
    }
}

async fn irc_connect(
    log: Logger,
    handler: CommandHandler,
    name: &str,
    mut config_update: ConfigMonitor,
) -> Result<bool, Error> {
    let config = config_update.current();

    let netconf = config
        .network
        .get(name)
        .ok_or_else(|| anyhow!("network shutdown"))?;

    info!(log, "connect"; "server" => &netconf.server, "port" => &netconf.port);

    let mut quitting = false;

    let mut client = Client::from_config(netconf.clone()).await?;
    client.identify()?;

    let mut stream = client.stream()?;
    let mut pending = FuturesUnordered::new();
    let quota = Quota::per_minute(nonzero!(10u32)); // Max of 10 per minute per channel
    let limiter = RateLimiter::keyed(quota);

    loop {
        tokio::select! {
            _newconf = config_update.next(), if !quitting => {
                let _ = client.send_quit("Disconnecting");
                quitting = true;
            },
            Some(futur) = pending.next() => {
                info!(log, "resolved"; "result" => ?futur);
            },
            message = stream.next() => {
                if message.is_none() {
                    break;
                }
                let message = message.unwrap();
                let message = message?;

                match &message.command {
                    Command::INVITE(target, channel) if target == client.current_nickname() && netconf.channels.contains(&channel) => {
                        info!(log, "invited"; "channel" => channel);
                        // TODO: channel keys
                        client.send_join(channel)?;
                    },
                    Command::KICK(channel, target, reason) if target == client.current_nickname() => {
                        info!(log, "kicked"; "channel" => channel, "reason" => reason);
                    }
                    Command::PRIVMSG(target, content) => {
                        if let Some(Prefix::Nickname(nick, _, _)) = &message.prefix {
                            if nick == client.current_nickname() || content.starts_with('\x01') || !netconf.channels.contains(&target) {
                                continue;
                            }

                            // TODO: Rate limit, deduplicate replies across requests
                            for url in url_entities(&content)
                                .into_iter()
                                .filter_map(|url| parse_url(url.substr(content)).ok())
                                .take(config.http.max_per_message as usize)
                                .unique()
                                {
                                if limiter.check_key(&target.clone()).is_err() {
                                    warn!(log, "ratelimit"; "channel" => target, "nick" => nick);
                                    break;
                                }

                                let urlstr = url.to_string();
                                let cmd = BotCommand::Url(url);
                                let sender = client.sender();
                                let logger = log.new(o!("url" => urlstr));
                                let target = target.clone();
                                let fut = handler.spawn(cmd).map_ok(move |res| {
                                    info!(logger, "resolve"; "result" => ?res);
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

    Ok(quitting)
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
