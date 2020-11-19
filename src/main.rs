use std::path::PathBuf;

use anyhow::Error;
use clap::Clap;
use futures::stream::FuturesUnordered;
use slog::{o, warn, Drain, Level};
use tokio::stream::StreamExt;

mod command;
mod config;
mod irc;
mod irc_string;
mod twitter;

use crate::{command::*, config::*, irc::*};

#[derive(Clap, Debug, Clone)]
struct Args {
    #[clap(short, long, default_value = "annoirc.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let args = Args::parse();

    let decorator = slog_term::TermDecorator::new().stdout().build();
    let drain = slog_term::FullFormat::new(decorator).build().fuse();
    let drain = slog_async::Async::new(drain)
        .build()
        .filter_level(Level::Debug)
        .fuse();
    let log = slog::Logger::root(drain, o!());

    warn!(log, "startup"; "version" => env!("CARGO_PKG_VERSION"), "config" => args.config.display(), "pid" => std::process::id());

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

    warn!(log, "exit");

    Ok(())
}
