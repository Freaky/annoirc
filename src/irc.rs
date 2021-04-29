use std::{sync::Arc, time::Duration};

use anyhow::Result;
use egg_mode_text::url_entities;
use futures::{stream::FuturesUnordered, TryFutureExt};
use governor::{Quota, RateLimiter};
use irc::client::prelude::*;
use itertools::Itertools;
use nonzero_ext::*;
use slog::{error, info, o, warn, Logger};
use tokio::{task::JoinHandle, time::Instant};
use tokio_stream::StreamExt;
use url::Url;

use crate::{command::*, config::*, irc_string::*, omdb::Movie, twitter::*, youtube::*};

#[derive(Debug)]
struct CommandResponse {
    log: Logger,
    target: String,
    info: Arc<Result<Info>>,
}

#[derive(Debug)]
pub struct IrcTask {
    name: String,
    log: Logger,
    handler: CommandHandler,
    config: ConfigMonitor,
    throttle: Backoff,
}

#[derive(Debug)]
struct Backoff {
    min: Duration,
    max: Duration,
    last_attempt: Option<Instant>,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            min: Duration::from_secs(10),
            max: Duration::from_secs(240),
            last_attempt: None,
        }
    }
}

// TODO: Add success/failure feedback. Not currently well defined by connect_loop
impl Backoff {
    fn next(&mut self) -> Option<Duration> {
        let now = Instant::now();
        let last = match self.last_attempt.replace(now) {
            None => return None,
            Some(attempt) => attempt,
        };

        let duration = now - last;
        let next_delay = if duration > self.max * 2 {
            self.min
        } else {
            duration.min(self.max / 2).max(self.min / 2) * 2
        };

        // Truncate to nearest second
        Some(next_delay - Duration::from_nanos(next_delay.subsec_nanos() as u64))
    }

    fn success(&mut self) {
        self.last_attempt = None;
    }
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
            throttle: Backoff::default(),
        };

        tokio::spawn(async move {
            s.connect_loop().await;
            s.name
        })
    }

    async fn connect_loop(&mut self) {
        let mut conf = self.config.clone();
        let mut delay = self.throttle.next();

        loop {
            tokio::select! {
                conn = self.connection(), if delay.is_none() => {
                    match conn {
                        Ok(exit) => {
                            warn!(self.log, "disconnected");

                            if exit {
                                break;
                            }
                        }
                        Err(e) => {
                            error!(self.log, "disconnected"; "error" => %e);
                        }
                    }

                    delay = self.throttle.next();
                    if let Some(delay) = delay {
                        info!(self.log, "sleep"; "delay" => ?delay);
                    }
                },
                _ = tokio::time::sleep(delay.unwrap_or_default()), if delay.is_some() => {
                    delay = None;
                },
                None = conf.next(), if delay.is_some() => {
                    break;
                }
            }
        }
    }

    async fn connection(&mut self) -> Result<bool> {
        let mut config = self.config.current();

        let netconf = config.network.get(&self.name);
        if netconf.is_none() {
            info!(self.log, "deconfigured");
            return Ok(true);
        }

        let netconf = netconf.unwrap().clone();

        warn!(self.log, "connect"; "server" => &netconf.server, "port" => &netconf.port);

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
                                warn!(self.log, "reconnecting");
                                client.send_quit("Reconnecting")?;
                            }
                        } else {
                            shutdown = true;
                            warn!(self.log, "deconfigured");
                            client.send_quit("Disconnecting")?;
                        }
                    } else {
                        shutdown = true;
                        warn!(self.log, "disconnecting");
                        client.send_quit("Disconnecting")?;
                    }
                },
                Some(fut) = pending.next() => { let _ = fut; /* probably cancelled by a concurrency change */ },
                message = stream.next() => {
                    if message.is_none() {
                        break;
                    }
                    let message = message.unwrap();
                    let message = message?;

                    match &message.command {
                        Command::ERROR(ref msg) => {
                            error!(self.log, "irc"; "error" => %msg);
                        },
                        Command::Response(irc::proto::Response::RPL_ENDOFMOTD, _)
                        | Command::Response(irc::proto::Response::ERR_NOMOTD, _) => {
                            self.throttle.success();
                            warn!(self.log, "connected"; "nick" => client.current_nickname());
                        },
                        Command::JOIN(ref c, None, None) => {
                            if let Some(Prefix::Nickname(nick, _, _)) = &message.prefix {
                                if nick == client.current_nickname() {
                                    warn!(self.log, "join"; "channel" => c);
                                }
                            }
                        }
                        Command::INVITE(target, channel) if target == client.current_nickname() && netconf.channels.contains(&channel) => {
                            warn!(self.log, "invited"; "channel" => channel, "source" => message_source(&message));
                            // TODO: channel keys
                            client.send_join(channel)?;
                        },
                        Command::KICK(channel, target, reason) if target == client.current_nickname() => {
                            warn!(self.log, "kicked"; "channel" => channel, "reason" => reason, "source" => message_source(&message));
                        },
                        Command::PRIVMSG(target, content) => {
                            if let Some(Prefix::Nickname(nick, _, _)) = &message.prefix {
                                // Avoid responding to ourselves, CTCPs, coloured text (usually other bots), and any target we're not configured for
                                if nick == client.current_nickname() || content.starts_with('\x01') || content.contains('\x03') || !netconf.channels.contains(&target) {
                                    continue;
                                }

                                if content.starts_with(&config.command.prefix) {
                                    let command = &content[config.command.prefix.len()..].split_whitespace().collect::<Vec<&str>>();
                                    if command.len() > 1 {
                                        let search = itertools::join(command[1..].iter(), " ");
                                        match &command[0].to_lowercase()[..] {
                                            "film" | "movie" if config.omdb.api_key.is_some() => {
                                                let cmd = BotCommand::Omdb("movie".to_string(), search.clone());
                                                info!(self.log, "omdb"; "kind" => "film", "search" => search, "channel" => %target, "source" => %nick);
                                                self.command(cmd, target.clone(), client.sender()).map(|fut| pending.push(fut));
                                                continue;
                                            }
                                            "show" | "series" | "tv" if config.omdb.api_key.is_some() => {
                                                let cmd = BotCommand::Omdb("series".to_string(), search.clone());
                                                info!(self.log, "omdb"; "kind" => "series", "search" => search, "channel" => %target, "source" => %nick);
                                                self.command(cmd, target.clone(), client.sender()).map(|fut| pending.push(fut));
                                                continue;
                                            }
                                            "wolfram" | "calc" if config.wolfram.app_id.is_some() => {
                                                let cmd = BotCommand::Wolfram(search.clone());
                                                info!(self.log, "wolfram"; "query" => search, "channel" => %target, "source" => %nick);
                                                self.command(cmd, target.clone(), client.sender()).map(|fut| pending.push(fut));
                                                continue;
                                            }
                                            _ => {}
                                        }
                                    }
                                }

                                for url in url_entities(&content)
                                    .into_iter()
                                    .filter(|url| !config.url.ignore_url_regex.is_match(url.substr(content)))
                                    .filter_map(|url| parse_url(url.substr(content), config.url.scheme_required).ok())
                                    .take(config.url.max_per_message as usize)
                                    .unique()
                                {
                                    if limiter.check_key(&target.clone()).is_err() {
                                        warn!(self.log, "ratelimit"; "channel" => target, "source" => nick);
                                        break;
                                    }

                                    if config.youtube.api_key.is_some() {
                                        if let Some(id) = extract_youtube_id(&url) {
                                            info!(self.log, "youtube"; "id" => %id, "channel" => %target, "source" => %nick);
                                            let cmd = BotCommand::YouTube(id);
                                            self.command(cmd, target.clone(), client.sender()).map(|fut| pending.push(fut));
                                            continue;
                                        }
                                    }

                                    let cmd = BotCommand::Url(url.clone());
                                    info!(self.log, "lookup"; "url" => %url, "channel" => %target, "source" => %nick);
                                    self.command(cmd, target.clone(), client.sender()).map(|fut| pending.push(fut));
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

    fn command(
        &self,
        cmd: BotCommand,
        target: String,
        sender: Sender,
    ) -> Option<
        impl futures::future::Future<Output = Result<Result<()>, futures::channel::oneshot::Canceled>>,
    > {
        let config = self.config.current();
        self.handler.spawn(cmd).map(move |fut| {
            fut.map_ok(move |res| {
                if let Ok(res) = &*res {
                    display_response(&res, &target, sender, config)
                } else {
                    Ok(())
                }
            })
        })
    }
}

fn message_source(msg: &Message) -> &str {
    match &msg.prefix {
        Some(Prefix::Nickname(nick, _, _)) => nick,
        Some(Prefix::ServerName(server)) => server,
        None => "unknown",
    }
}

fn display_response(
    info: &Info,
    target: &str,
    sender: Sender,
    config: Arc<BotConfig>,
) -> Result<()> {
    match &info {
        Info::Url(info) => {
            let host = sanitize(info.url.host_str().unwrap_or(""), 30);
            sender.send_privmsg(
                &target,
                format!(
                    "[\x0303\x02\x02{}\x0f] \x0300\x02\x02{}\x0f",
                    host,
                    info.title.trunc(380)
                ),
            )?;
            if let (true, Some(desc)) = (config.url.include_description, &info.desc) {
                sender.send_privmsg(
                    &target,
                    format!(
                        "[\x0303{}\x02\x02\x0f] \x0300\x02\x02{}\x0f",
                        host,
                        desc.trunc(380)
                    ),
                )?;
            }
        }
        Info::Tweet(tweet) => {
            sender.send_privmsg(&target, format_tweet(tweet, "Twitter"))?;
            if let Some(quote) = &tweet.quote {
                sender.send_privmsg(&target, format_tweet(quote, "Retweet"))?;
            }
            if let Some(retweet) = &tweet.retweet {
                sender.send_privmsg(&target, format_tweet(retweet, "Retweet"))?;
            }
        }
        Info::Tweeter(user) => {
            sender.send_privmsg(&target, format_tweeter(user))?;
            if let Some(tweet) = &user.status {
                sender.send_privmsg(&target, format_tweet(tweet, " Status"))?;
            }
        }
        Info::Movie(movie) => {
            sender.send_privmsg(&target, format_movie(movie))?;
        }
        Info::YouTube(item) => {
            sender.send_privmsg(&target, format_youtube(item))?;
        }
        Info::Wolfram(response) => {
            sender.send_privmsg(&target, format!("[\x0303\x02\x02WolframAlpha\x0f] \x0300\x02\x02{}\x0f", response))?;
        }
    }

    Ok(())
}

fn format_movie(movie: &Movie) -> String {
    format!(
        "[\x0303{}\x0f] \x0304{}\x0f ({}) [{}/10 with {} votes, Metascore: {}] [{}] [{}] \x0303{}\x0f - \x0300\x02\x02{}\x0f",
        "IMDB",
        movie.title.trunc(30),
        movie.released,
        movie.imdb_rating,
        movie.imdb_votes,
        movie.metascore,
        movie.rated,
        movie.genre,
        format!("https://www.imdb.com/title/{}", movie.imdb_id),
        movie.plot,
    )
}

fn format_tweet(tweet: &Tweet, tag: &str) -> String {
    // not included if retrieved from a user status field
    if let Some(user) = &tweet.user {
        format!(
            "[\x0303{}\x0f] \x0304\x02\x02{}\x0f{} (@{}) \x0300\x02\x02{}\x0f | {} {}",
            tag,
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
            "[\x0303{}\x0f] \x0300\x02\x02{}\x0f | {} {}",
            tag,
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

fn format_youtube(item: &YouTube) -> String {
    let duration = item.duration;
    let seconds = duration.as_secs() % 60;
    let minutes = (duration.as_secs() / 60) % 60;
    let hours = (duration.as_secs() / 60) / 60;

    let duration = if hours > 0 {
        format!("{}:{}:{:02}", hours, minutes, seconds)
    } else {
        format!("{}:{:02}", minutes, seconds)
    };

    format!(
        "[\x0303YouTube\x0f] \x0304\x02\x02{title}\x0f - \"\x0300\x02\x02{desc}\x0f\", {views} views, +{likes}/-{dislikes}, [{duration}] - https://youtu.be/{id}",
        title = item.title.trunc(40),
        desc = item.description.trunc(200),
        views = item.views,
        likes = item.likes,
        dislikes = item.dislikes,
        duration = duration,
        id = item.id
    )
}

fn parse_url(text: &str, scheme_required: bool) -> Result<Url, url::ParseError> {
    match Url::parse(text) {
        Ok(url) => Ok(url),
        Err(url::ParseError::RelativeUrlWithoutBase) if !scheme_required => {
            Url::parse(&format!("http://{}", text))
        }
        Err(e) => Err(e),
    }
}
