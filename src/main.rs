use std::net::ToSocketAddrs;
use std::path::PathBuf;

use anyhow::{format_err, Context, Result};
use prettytable::{cell, format, row, Table};
use serde_derive::Deserialize;
use seymour_protocol::{Command, Response};
use structopt::StructOpt;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader, Lines, ReadHalf, WriteHalf,
};
use tokio::net::TcpStream;

#[derive(Debug, Deserialize)]
struct Config {
    host_port: String,
    user: String,
}

#[derive(Debug, StructOpt)]
enum Subcommand {
    /// List unread feed entries
    Unread {
        /// Don't mark entries as read as they're listed
        #[structopt(long)]
        no_mark_read: bool,
    },

    /// List all subscriptions
    #[structopt(alias = "subscriptions")]
    ListSubscriptions,
}

#[derive(Debug, StructOpt)]
#[structopt(
    name = "seymour-cli",
    about = "A simple client for the seymour gemini feed aggregator"
)]
struct Args {
    /// Configuration file. ~/.config/seymour-cli/config.toml if not present.
    #[structopt(long, parse(from_os_str))]
    config_file: Option<PathBuf>,

    #[structopt(subcommand)]
    subcommand: Subcommand,
}

async fn send<T: AsyncWrite>(writer: &mut WriteHalf<T>, command: Command) -> Result<()> {
    Ok(writer
        .write_all(format!("{}\r\n", command).as_bytes())
        .await?)
}

async fn receive<T: AsyncBufRead + Unpin>(lines: &mut Lines<T>) -> Result<Response> {
    Ok(lines
        .next_line()
        .await?
        .ok_or_else(|| format_err!("no line from server"))?
        .parse()?)
}

#[derive(Debug)]
struct Entry {
    id: i64,
    full_url: String,
    title: String,
}

#[derive(Debug)]
struct Subscription {
    id: i64,
    url: String,
}

macro_rules! check_response {
    ($variant:pat, $response:expr) => {
        match $response {
            $variant => {}
            _ => {
                return Err(format_err!(
                    "unexpected response (expected {:?}): {}",
                    stringify!($variant),
                    $response
                ));
            }
        }
    };
}

async fn connect(
    config: &Config,
) -> Result<(Lines<BufReader<ReadHalf<TcpStream>>>, WriteHalf<TcpStream>)> {
    let address = config
        .host_port
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| format_err!("missing server address"))?;
    let stream = TcpStream::connect(&address).await?;

    let (reader, writer) = tokio::io::split(stream);

    let server_reader = BufReader::new(reader);
    let lines = server_reader.lines();

    Ok((lines, writer))
}

async fn cmd_unread(config: Config, no_mark_read: bool) -> Result<()> {
    let (mut lines, mut writer) = connect(&config).await?;

    send(
        &mut writer,
        Command::User {
            username: config.user,
        },
    )
    .await?;

    let response: Response = receive(&mut lines).await?;
    check_response!(Response::AckUser { .. }, response);

    send(&mut writer, Command::ListUnread).await?;

    let response: Response = receive(&mut lines).await?;
    check_response!(Response::StartEntryList, response);

    let mut entries = Vec::new();
    loop {
        let response: Response = receive(&mut lines).await?;
        match response {
            Response::Entry {
                feed_url,
                url,
                title,
                id,
                ..
            } => {
                entries.push(Entry {
                    id,
                    title,
                    full_url: format!("{}/{}", feed_url, url),
                });
            }
            Response::EndList => {
                break;
            }
            _ => {
                return Err(format_err!(
                    "unexpected response (expected Entry or EndList): {}",
                    response
                ));
            }
        }
    }

    if entries.is_empty() {
        println!("No new items");
        return Ok(());
    }

    println!("{} new item(s)", entries.len());

    let mut table = Table::new();
    table.set_format(*format::consts::FORMAT_NO_BORDER_LINE_SEPARATOR);

    table.set_titles(row!["url", "title"]);

    for entry in entries {
        table.add_row(row![entry.full_url, entry.title]);

        if !no_mark_read {
            send(&mut writer, Command::MarkRead { id: entry.id }).await?;

            let response: Response = receive(&mut lines).await?;
            check_response!(Response::AckMarkRead, response);
        }
    }

    table.printstd();

    Ok(())
}

async fn cmd_list_subscriptions(config: Config) -> Result<()> {
    let (mut lines, mut writer) = connect(&config).await?;

    send(
        &mut writer,
        Command::User {
            username: config.user,
        },
    )
    .await?;

    let response: Response = receive(&mut lines).await?;
    check_response!(Response::AckUser { .. }, response);

    send(&mut writer, Command::ListSubscriptions).await?;

    let response: Response = receive(&mut lines).await?;
    check_response!(Response::StartSubscriptionList, response);

    let mut subscriptions = Vec::new();
    loop {
        let response: Response = receive(&mut lines).await?;
        match response {
            Response::Subscription { id, url } => {
                subscriptions.push(Subscription { id, url });
            }
            Response::EndList => {
                break;
            }
            _ => {
                return Err(format_err!(
                    "unexpected response (expected Subscription or EndList): {}",
                    response
                ));
            }
        }
    }

    if subscriptions.is_empty() {
        println!("No subscriptions");
    }

    let mut table = Table::new();
    table.set_format(*format::consts::FORMAT_NO_BORDER_LINE_SEPARATOR);

    table.set_titles(row!["url"]);

    for entry in subscriptions {
        table.add_row(row![entry.url]);
    }

    table.printstd();

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::from_args();

    let config_file = match args.config_file {
        Some(config_file) => config_file,
        None => {
            let dirs = xdg::BaseDirectories::with_prefix("seymour-cli")?;
            dirs.find_config_file("config.toml").ok_or_else(|| {
                format_err!("no seymour-cli config file found in .config/seymour-cli")
            })?
        }
    };

    let config: Config = toml::from_str(
        &std::fs::read_to_string(config_file.clone())
            .with_context(|| format_err!("failed to read config file at {:?}", config_file))?,
    )
    .with_context(|| format_err!("failed to parse config file at {:?}", config_file))?;

    match args.subcommand {
        Subcommand::Unread { no_mark_read } => cmd_unread(config, no_mark_read).await,
        Subcommand::ListSubscriptions => cmd_list_subscriptions(config).await,
    }
}
