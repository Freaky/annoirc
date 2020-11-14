use std::time::Duration;

use anyhow::{anyhow, Error};
use egg_mode::tweet;
use egg_mode_text::url_entities;
use futures::stream::FuturesUnordered;
use futures::TryFutureExt;
use irc::client::prelude::*;
use itertools::Itertools;
use lazy_static::lazy_static;
use regex::Regex;
use slog::{error, info, o, Logger};
use tokio::stream::StreamExt;
use url::Url;

use crate::command::*;
use crate::config::*;

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
                            if nick == client.current_nickname() || content.starts_with("\x01") || !netconf.channels.contains(&target) {
                                continue;
                            }

                            // TODO: Rate limit, deduplicate replies across requests
                            for url in url_entities(&content)
                                .into_iter()
                                .filter_map(|url| parse_url(url.substr(content)).ok())
                                .unique()
                                {
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
                                                    sanitize(&info.title, 380)
                                                )
                                            );
                                            if let Some(desc) = &info.desc {
                                                let _ = sender.send_privmsg(
                                                    &target,
                                                    format!(
                                                        "[\x0303{}\x02\x02\x0f] \x0300\x02\x02{}\x0f",
                                                        sanitize(info.url.host_str().unwrap_or(""), 30),
                                                        sanitize(&desc, 380)
                                                    )
                                                );
                                            }
                                        },
                                        Ok(Info::Tweet(ref tweet)) => {
                                            let _ = sender.send_privmsg(
                                                &target,
                                                format_tweet(tweet)
                                            );
                                            if let Some(quote) = &tweet.quoted_status {
                                                let _ = sender.send_privmsg(
                                                    &target,
                                                    format_tweet(quote)
                                                );
                                            }
                                            if let Some(retweet) = &tweet.retweeted_status {
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

fn format_tweet(tweet: &tweet::Tweet) -> String {
    // not included if retrieved from a user status field
    if let Some(user) = &tweet.user {
        format!(
            "[\x0303Twitter\x0f] \x0304\x02\x02{}\x0f{} (@{}) \x0300\x02\x02{}\x0f | {} {}",
            sanitize(&user.name, 30),
            if user.verified { "✓" } else { "" },
            sanitize(&user.screen_name, 30),
            sanitize(&tweet.text, 300),
            if tweet.favorite_count == 0 {
                "".to_string()
            } else {
                format!("❤️{}", tweet.favorite_count)
            },
            tweet.created_at.format("%F %H:%M")
        )
    } else {
        format!(
            "[\x0303Twitter\x0f] \x0300\x02\x02{}\x0f | {} {}",
            sanitize(&tweet.text, 300),
            if tweet.favorite_count == 0 {
                "".to_string()
            } else {
                format!("❤️{}", tweet.favorite_count)
            },
            tweet.created_at.format("%F %H:%M")
        )
    }
}

fn format_tweeter(user: &egg_mode::user::TwitterUser) -> String {
    format!(
        "[\x0303Twitter\x0f] \x0304\x02\x02{}\x0f{} (@{}) {} Tweets, {} Followers, {}{}",
        sanitize(&user.name, 30),
        if user.verified { "✓" } else { "" },
        sanitize(&user.screen_name, 30),
        user.statuses_count,
        user.followers_count,
        if let Some(ref desc) = user.description {
            format!("\"\x0300\x02\x02{}\x0f\", ", sanitize(desc, 300))
        } else {
            "".to_string()
        },
        user.created_at.format("%F %H:%M")
    )
}

fn sanitize(text: &str, max_bytes: usize) -> String {
    lazy_static! {
        // Collapse any whitespace to a single space
        static ref WHITESPACE: Regex = Regex::new(r"\s+").unwrap();

        // Strip control codes and multiple combining chars
        static ref CONTROL: Regex = Regex::new(r"\pC|(?:\pM{2})\pM+").unwrap();
    }

    let text = WHITESPACE.replace_all(text, " ");
    let text = CONTROL.replace_all(&text, "");

    // Avoid cutting codepoints in half
    text.char_indices()
        .take_while(|(idx, c)| *idx + c.len_utf8() <= max_bytes)
        .map(|(_, c)| c)
        .collect()
}

fn parse_url(text: &str) -> Result<Url, url::ParseError> {
    match Url::parse(text) {
        Ok(url) => Ok(url),
        Err(url::ParseError::RelativeUrlWithoutBase) => Url::parse(&format!("http://{}", text)),
        Err(e) => Err(e),
    }
}
