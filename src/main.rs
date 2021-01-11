use std::net::ToSocketAddrs;
use std::path::PathBuf;

use anyhow::{format_err, Context, Result};
use prettytable::{cell, format, row, Table};
use serde_derive::Deserialize;
use seymour_protocol::{Command, Response};
use structopt::StructOpt;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader, Lines, WriteHalf,
};
use tokio::net::TcpStream;

#[derive(Debug, Deserialize)]
struct Config {
    host_port: String,
    user: String,
}

#[derive(Debug, StructOpt)]
enum Subcommand {
    Unread,
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

async fn cmd_unread(config: Config) -> Result<()> {
    let address = config
        .host_port
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| format_err!("missing server address"))?;
    let stream = TcpStream::connect(&address).await?;

    let (reader, mut writer) = tokio::io::split(stream);

    let server_reader = BufReader::new(reader);
    let mut lines = server_reader.lines();

    send(
        &mut writer,
        Command::User {
            username: config.user,
        },
    )
    .await?;

    let response: Response = receive(&mut lines).await?;
    match response {
        Response::AckUser { .. } => {}
        _ => {
            return Err(format_err!(
                "unexpected response (expected AckUser): {}",
                response
            ));
        }
    }

    send(&mut writer, Command::ListUnread).await?;

    let response: Response = receive(&mut lines).await?;
    match response {
        Response::StartEntryList => {}
        _ => {
            return Err(format_err!(
                "unexpected response (expected StartEntryList): {}",
                response
            ));
        }
    }

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
                    "unexpected response (expected StartEntryList): {}",
                    response
                ));
            }
        }
    }

    if entries.is_empty() {
        println!("No new items");
    } else {
        println!("{} new item(s)", entries.len());

        let mut table = Table::new();
        table.set_format(*format::consts::FORMAT_NO_BORDER_LINE_SEPARATOR);

        table.set_titles(row!["url", "title"]);

        for entry in entries {
            table.add_row(row![entry.full_url, entry.title]);

            send(&mut writer, Command::MarkRead { id: entry.id }).await?;

            let response: Response = receive(&mut lines).await?;
            match response {
                Response::AckMarkRead => {}
                _ => {
                    return Err(format_err!(
                        "unexpected response (expected AckMarkRead): {}",
                        response
                    ));
                }
            }
        }

        table.printstd();
    }

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
        Subcommand::Unread => cmd_unread(config).await,
    }
}
