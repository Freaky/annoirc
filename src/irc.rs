use std::{sync::Arc, time::Duration};

use anyhow::Result;
use egg_mode_text::url_entities;
use futures::{stream::FuturesUnordered, TryFutureExt};
use governor::{Quota, RateLimiter};
use irc::client::prelude::*;
use itertools::Itertools;
use nonzero_ext::*;
use num_format::{Locale, ToFormattedString};
use slog::{error, info, o, warn, Logger};
use tokio::{task::JoinHandle, time::Instant};
use tokio_stream::StreamExt;
use url::Url;

use crate::{
    command::*, config::*, irc_string::*, omdb::Movie, wolfram::WolframPod, youtube::*,
};

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
                        Command::INVITE(target, channel) if target == client.current_nickname() && netconf.channels.contains(channel) => {
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
                                if nick == client.current_nickname() || content.starts_with('\x01') || content.contains('\x03') || !netconf.channels.contains(target) {
                                    continue;
                                }

                                if content.starts_with(&config.command.prefix) {
                                    let split = &mut content[config.command.prefix.len()..].split_ascii_whitespace();
                                    let command = split.next().unwrap_or_default().to_lowercase().to_string();
                                    let args = itertools::join(split, " ");
                                    if !command.is_empty() && !args.is_empty() {
                                        if config.omdb.api_key.is_some() {
                                            let kind = match &command[..] {
                                                "imdb" | "omdb" => Some("Any"),
                                                "film" | "movie" => Some("Movie"),
                                                "show" | "series" | "tv" => Some("Series"),
                                                "ep" | "episode" => Some("Episode"),
                                                "game" => Some("Game"),
                                                _ => None,
                                            };

                                            if let Some(kind) = kind {
                                                if limiter.check_key(&target.clone()).is_err() {
                                                    warn!(self.log, "ratelimit"; "channel" => target, "source" => nick);
                                                    continue;
                                                }

                                                info!(self.log, "omdb"; "kind" => kind, "search" => &args, "channel" => %target, "source" => %nick);
                                                self.command(BotCommand::Omdb(kind, args.clone()), target.clone(), client.sender()).map(|fut| pending.push(fut));
                                                continue;
                                            }
                                        }
                                        if config.wolfram.app_id.is_some() && matches!(&command[..], "wolfram" | "calc") {
                                            if limiter.check_key(&target.clone()).is_err() {
                                                warn!(self.log, "ratelimit"; "channel" => target, "source" => nick);
                                                continue;
                                            }

                                            info!(self.log, "wolfram"; "query" => &args, "channel" => %target, "source" => %nick);
                                            self.command(BotCommand::Wolfram(args.clone()), target.clone(), client.sender()).map(|fut| pending.push(fut));
                                            continue;
                                        }
                                    }
                                }

                                for url in url_entities(content)
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
                    display_response(res, &target, sender, config)
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
                target,
                format!(
                    "[\x0303\x02\x02{}\x0f] \x0300\x02\x02{}\x0f",
                    host,
                    info.title.trunc(380)
                ),
            )?;
            if let (true, Some(desc)) = (config.url.include_description, &info.desc) {
                sender.send_privmsg(
                    target,
                    format!(
                        "[\x0303{}\x02\x02\x0f] \x0300\x02\x02{}\x0f",
                        host,
                        desc.trunc(380)
                    ),
                )?;
            }
        }
        Info::Movie(movie) => {
            sender.send_privmsg(target, format_movie(movie))?;
        }
        Info::YouTube(item) => {
            sender.send_privmsg(target, format_youtube(item))?;
        }
        Info::Wolfram(response) => {
            for pod in format_wolfram(response) {
                sender.send_privmsg(target, pod)?;
            }
        }
    }

    Ok(())
}

fn format_movie(movie: &Movie) -> String {
    format!(
        "[\x0303IMDB\x0f] \x0304{title}\x0f ({released}) [{rating}/10 with {votes} votes, Metascore: {metascore}] [{rated}] [{genre}] \x0303https://www.imdb.com/title/{imdb_id}\x0f - \x0300\x02\x02{plot}\x0f",
        title = movie.title.trunc(30),
        released = movie.released,
        rating = movie.imdb_rating,
        votes = movie.imdb_votes,
        metascore = movie.metascore,
        rated = movie.rated,
        genre = movie.genre,
        imdb_id = movie.imdb_id,
        plot = movie.plot,
    )
}

fn format_youtube(item: &YouTube) -> String {
    let duration = item.duration;
    let seconds = duration.as_secs() % 60;
    let minutes = (duration.as_secs() / 60) % 60;
    let hours = (duration.as_secs() / 60) / 60;

    let duration = if hours > 0 {
        format!("{}:{:02}:{:02}", hours, minutes, seconds)
    } else {
        format!("{}:{:02}", minutes, seconds)
    };

    format!(
        "[\x0303{channel}\x0f{date}] \x0304\x02\x02{title}\x0f - \"\x0300\x02\x02{desc}\x0f\" [{duration}] {views} views ❤️{likes}",
        title = item.title.trunc(40),
        desc = item.description.trunc(200),
        channel = item.channel.trunc(16),
        views = item.views.to_formatted_string(&Locale::en),
        likes = item.likes.to_formatted_string(&Locale::en),
        date = item.published_at.map(|d| d.format(" @ %F").to_string()).unwrap_or_default(),
        duration = duration,
    )
}

fn format_wolfram(pods: &[WolframPod]) -> Vec<String> {
    pods.iter()
        .take(3)
        .map(|pod| {
            format!(
                "[\x0303WolframAlpha\x0f] \x0304\x02\x02{title}\x0f: \x0300\x02\x02{value}\x0f",
                title = pod.title.trunc(40),
                value = pod.values[0].trunc(200),
            )
        })
        .collect()
}

fn parse_url(text: &str, scheme_required: bool) -> Result<Url, url::ParseError> {
    match Url::parse(text) {
        Ok(mut url) => {
            if let Some("twitter.com") = url.host_str() {
                let _ = url.set_host(Some("uk.unofficialbird.com"));
            }
            Ok(url)
        },
        Err(url::ParseError::RelativeUrlWithoutBase) if !scheme_required => {
            Url::parse(&format!("http://{}", text))
        }
        Err(e) => Err(e),
    }
}
