use anyhow::{anyhow, Result};
use clap::{CommandFactory, Parser};

mod config;
mod daemon;
mod player;
mod protocol;
mod subsonic;

use config::Config;
use protocol::Request;
use subsonic::NavidromeClient;

#[derive(Parser, Debug)]
#[command(name = "simplay", version, about = "Headless Navidrome/Subsonic backend player")]
struct Cli {
    #[arg(long, short = 'd', help = "Run the simplay daemon")]
    daemon: bool,
    #[arg(long, short = 'C', help = "Configure simplay")]
    configure: bool,

    #[arg(long, short = 's', help = "Shuffle the library")]
    shuffle: bool,
    #[arg(long, short = 'p', help = "Pause playback")]
    pause: bool,
    #[arg(long, short = 'P', help = "Resume playback")]
    play: bool,
    #[arg(long, short = 'f', help = "Play next track")]
    fastforward: bool,
    #[arg(long, short = 'r', help = "Play previous track")]
    rewind: bool,
    #[arg(long, short = 'o', help = "Restart current track")]
    startover: bool,
    #[arg(long, short = 'l', help = "Heart current song")]
    likesong: bool,
    #[arg(long, short = 'u', help = "Unheart current song")]
    unlikesong: bool,
    #[arg(long, short = 'R', value_name = "1-5", help = "Rate current song (1-5)")]
    rate: Option<u8>,
    #[arg(long, short = 'v', help = "Increase volume")]
    volumeup: bool,
    #[arg(long, short = 'V', help = "Decrease volume")]
    volumedown: bool,
    #[arg(long, short = 'H', help = "Shuffle liked (hearted) songs")]
    shuffleliked: bool,
    #[arg(long, short = 't', help = "Show playback status")]
    status: bool,

    #[arg(long, short = 'a', value_name = "ARTIST", help = "Shuffle artist")]
    shuffleartist: Option<String>,
    #[arg(long, short = 'b', value_name = "ALBUM", help = "Shuffle album")]
    shufflealbum: Option<String>,
    #[arg(long, short = 'g', value_name = "PLAYLIST", help = "Shuffle playlist")]
    shuffleplaylist: Option<String>,
    #[arg(long, short = 'A', value_name = "ALBUM", help = "Play album")]
    playalbum: Option<String>,
    #[arg(long, short = 'c', value_name = "PLAYLIST", help = "Add current song to playlist")]
    addsongtoplaylist: Option<String>,
    #[arg(long, short = 'D', value_name = "PLAYLIST", help = "Delete playlist")]
    deleteplaylist: Option<String>,

    #[arg(long, value_name = "ENDPOINT", help = "Raw Subsonic API call")]
    api: Option<String>,
    #[arg(long, value_name = "KEY=VALUE", help = "Parameter for --api", action = clap::ArgAction::Append)]
    param: Vec<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.configure {
        Config::configure()?;
        println!("simplay configured");
        return Ok(());
    }

    if cli.daemon {
        let config = Config::load_or_prompt_required()?;
        return daemon::run(config);
    }

    if let Some(endpoint) = cli.api.as_deref() {
        let config = Config::load_or_prompt_required()?;
        return run_api_call(&config, endpoint, &cli.param);
    }

    let req = build_request(&cli)?;
    if req.is_none() {
        Cli::command().print_help()?;
        println!();
        return Ok(());
    }

    let socket_path = Config::socket_path()?;
    let req = req.unwrap();
    let resp = match protocol::send_request(&socket_path, &req) {
        Ok(resp) => resp,
        Err(err) => {
            eprintln!("simplay: daemon not running or socket unavailable: {}", err);
            eprintln!(
                "hint: run `simplay --daemon` (remove stale socket {} if needed)",
                socket_path.display()
            );
            std::process::exit(1);
        }
    };

    if !resp.ok {
        eprintln!("simplay: {}", resp.message);
        std::process::exit(1);
    }

    if let Some(status) = resp.status {
        if let Some(song) = status.song {
            let state = if status.paused { "paused" } else { "playing" };
            println!("{}: {} - {} ({})", state, song.artist, song.title, song.album);
            println!("queue: {} | index: {}", status.queue_len, status.index);
        } else {
            println!("idle");
        }
    } else {
        println!("{}", resp.message);
    }

    Ok(())
}

fn build_request(cli: &Cli) -> Result<Option<Request>> {
    let mut requests = Vec::new();

    if cli.shuffle {
        requests.push(Request::new("shuffle", None));
    }
    if cli.pause {
        requests.push(Request::new("pause", None));
    }
    if cli.play {
        requests.push(Request::new("play", None));
    }
    if cli.fastforward {
        requests.push(Request::new("fastforward", None));
    }
    if cli.rewind {
        requests.push(Request::new("rewind", None));
    }
    if cli.startover {
        requests.push(Request::new("startover", None));
    }
    if cli.likesong {
        requests.push(Request::new("likesong", None));
    }
    if cli.unlikesong {
        requests.push(Request::new("unlikesong", None));
    }
    if let Some(rating) = cli.rate {
        if !(1..=5).contains(&rating) {
            return Err(anyhow!("Rating must be between 1 and 5"));
        }
        requests.push(Request::new("rate", Some(rating.to_string())));
    }
    if cli.volumeup {
        requests.push(Request::new("volumeup", None));
    }
    if cli.volumedown {
        requests.push(Request::new("volumedown", None));
    }
    if cli.shuffleliked {
        requests.push(Request::new("shuffleliked", None));
    }
    if cli.status {
        requests.push(Request::new("status", None));
    }

    if let Some(artist) = cli.shuffleartist.clone() {
        requests.push(Request::new("shuffleartist", Some(artist)));
    }
    if let Some(album) = cli.shufflealbum.clone() {
        requests.push(Request::new("shufflealbum", Some(album)));
    }
    if let Some(playlist) = cli.shuffleplaylist.clone() {
        requests.push(Request::new("shuffleplaylist", Some(playlist)));
    }
    if let Some(album) = cli.playalbum.clone() {
        requests.push(Request::new("playalbum", Some(album)));
    }
    if let Some(playlist) = cli.addsongtoplaylist.clone() {
        requests.push(Request::new("addsongtoplaylist", Some(playlist)));
    }
    if let Some(playlist) = cli.deleteplaylist.clone() {
        requests.push(Request::new("deleteplaylist", Some(playlist)));
    }

    if requests.len() > 1 {
        return Err(anyhow!("Only one action can be specified at a time"));
    }

    Ok(requests.pop())
}

fn run_api_call(config: &Config, endpoint: &str, params: &[String]) -> Result<()> {
    let client = NavidromeClient::new(config)?;
    let mut extra = Vec::new();
    for param in params {
        let (key, value) = split_param(param)?;
        extra.push((key, value));
    }
    let json = client.request(endpoint, &extra)?;
    let output = serde_json::to_string_pretty(&json)?;
    println!("{}", output);
    Ok(())
}

fn split_param(param: &str) -> Result<(&str, String)> {
    let mut parts = param.splitn(2, '=');
    let key = parts.next().unwrap_or("");
    let value = parts.next().unwrap_or("");
    if key.is_empty() || value.is_empty() {
        return Err(anyhow!("Invalid param {}, expected key=value", param));
    }
    Ok((key, value.to_string()))
}
