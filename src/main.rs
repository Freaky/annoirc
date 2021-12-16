use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use futures::stream::FuturesUnordered;
use slog::{crit, o, warn, Drain, Level, Logger};
use tokio_stream::StreamExt;

mod command;
mod config;
mod irc;
mod irc_string;
mod omdb;
mod twitter;
mod wolfram;
mod youtube;

use crate::{command::*, config::*, irc::*};

#[derive(Parser, Debug, Clone)]
struct Args {
    #[clap(short, long, default_value = "annoirc.toml")]
    config: PathBuf,
}

async fn run(args: Args, log: Logger) -> Result<()> {
    let mut config_update = ConfigMonitor::watch(log.clone(), &args.config).await?;
    let mut config = config_update.current();

    let handler = CommandHandler::new(log.clone(), config_update.clone());
    let mut networks = std::collections::HashSet::<String>::new();
    let mut connections = FuturesUnordered::new();
    let mut active = true;

    loop {
        if active {
            for netname in config.network.keys() {
                if !networks.contains(netname) {
                    networks.insert(netname.clone());
                    connections.push(IrcTask::spawn(
                        log.clone(),
                        handler.clone(),
                        config_update.clone(),
                        netname.clone(),
                    ));
                }
            }
        }

        tokio::select! {
            conf = config_update.next(), if active => {
                if let Some(conf) = conf {
                    config = conf;
                } else {
                    active = false;
                }
            },
            Some(connection) = connections.next() => {
                let network = connection.expect("Shouldn't panic");
                networks.remove(&network);
            },
            else => break
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let ec = {
        let decorator = slog_term::TermDecorator::new().stdout().build();
        let drain = slog_term::FullFormat::new(decorator).build().fuse();
        let drain = slog_async::Async::new(drain)
            .build()
            .filter_level(Level::Debug)
            .fuse();
        let log = slog::Logger::root(drain, o!());

        warn!(log, "startup"; "version" => env!("CARGO_PKG_VERSION"), "config" => args.config.display(), "pid" => std::process::id());

        if let Err(e) = run(args, log.clone()).await {
            crit!(log, "exit"; "error" => %e);
            1
        } else {
            warn!(log, "exit");
            0
        }
    };

    std::process::exit(ec);
}
