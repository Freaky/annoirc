use std::path::PathBuf;

use anyhow::Error;
use clap::Clap;
use futures::FutureExt;
use slog::{info, o, Drain, Level};
use tokio::stream::{StreamExt, StreamMap};

mod command;
mod config;
mod irc;

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

    info!(log, "startup"; "version" => env!("CARGO_PKG_VERSION"), "config" => args.config.display());

    let mut config_update = BotConfig::watch(
        log.new(o!("config" => args.config.display().to_string())),
        &args.config,
    )
    .await?;
    let mut config = config_update.recv().await.unwrap();

    let handler = CommandHandler::new(config_update.clone());
    let mut connections = StreamMap::new();

    loop {
        for (netname, _) in &config.network {
            if !connections.contains_key(netname) {
                info!(log, "spawn"; "name" => netname);
                let name = netname.clone();
                let cu = config_update.clone();
                let newlog = log.new(o!("network" => name.clone()));
                connections.insert(
                    netname.clone(),
                    tokio::spawn(irc_instance(newlog, handler.clone(), name, cu)).into_stream(),
                );
            }
        }

        if connections.is_empty() {
            break;
        }

        tokio::select! {
            Some(conf) = config_update.recv() => {
                config = conf;
            },
            Some((network, connection)) = connections.next() => {
                info!(log, "close"; "network" => &network, "result" => ?connection);
                connections.remove(&network);
            },
            else => break
        }
    }

    info!(log, "shutdown");

    Ok(())
}
