use crate::config::Config;
use anyhow::{anyhow, Context, Result};
use rand::distributions::Alphanumeric;
use rand::Rng;
use reqwest::blocking::Client;
use serde_json::Value;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Song {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: Option<u32>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct Item {
    pub id: String,
    pub name: String,
}

#[derive(Clone)]
pub struct NavidromeClient {
    base_url: String,
    username: String,
    password: String,
    api_version: String,
    client_name: String,
    endpoint_suffix: String,
    http: Client,
}

impl NavidromeClient {
    pub fn new(config: &Config) -> Result<Self> {
        let mut builder = Client::builder();
        if !config.tls_verify {
            builder = builder.danger_accept_invalid_certs(true);
        }
        let http = builder.timeout(Duration::from_secs(20)).build()?;
        Ok(Self {
            base_url: config.server_url.clone(),
            username: config.username.clone(),
            password: config.password.clone(),
            api_version: config.api_version.clone(),
            client_name: config.client_name.clone(),
            endpoint_suffix: config.endpoint_suffix.clone(),
            http,
        })
    }

    pub fn request(&self, endpoint: &str, extra_params: &[(&str, String)]) -> Result<Value> {
        let url = format!(
            "{}/rest/{}.{}",
            self.base_url.trim_end_matches('/'),
            endpoint,
            self.endpoint_suffix
        );
        let (token, salt) = self.token_pair();
        let mut params = vec![
            ("u", self.username.clone()),
            ("t", token),
            ("s", salt),
            ("v", self.api_version.clone()),
            ("c", self.client_name.clone()),
            ("f", "json".to_string()),
        ];
        for (k, v) in extra_params {
            params.push((*k, v.clone()));
        }

        let resp = self
            .http
            .get(url)
            .query(&params)
            .send()
            .with_context(|| format!("Failed request {}", endpoint))?
            .error_for_status()?;
        let json: Value = resp.json()?;
        let status = json
            .get("subsonic-response")
            .and_then(|v| v.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("failed");
        if status != "ok" {
            let err = json
                .get("subsonic-response")
                .and_then(|v| v.get("error"))
                .and_then(|v| v.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown error");
            return Err(anyhow!(err.to_string()));
        }
        Ok(json)
    }

    pub fn stream_url(&self, song_id: &str) -> Result<String> {
        let url = format!(
            "{}/rest/stream.{}",
            self.base_url.trim_end_matches('/'),
            self.endpoint_suffix
        );
        let mut url = reqwest::Url::parse(&url)?;
        let (token, salt) = self.token_pair();
        url.query_pairs_mut()
            .append_pair("u", &self.username)
            .append_pair("t", &token)
            .append_pair("s", &salt)
            .append_pair("v", &self.api_version)
            .append_pair("c", &self.client_name)
            .append_pair("id", song_id);
        Ok(url.to_string())
    }

    pub fn get_random_songs(&self, size: usize) -> Result<Vec<Song>> {
        let json = self.request("getRandomSongs", &[("size", size.to_string())])?;
        let songs = json
            .get("subsonic-response")
            .and_then(|v| v.get("randomSongs"))
            .and_then(|v| v.get("song"))
            .map(parse_song_list)
            .unwrap_or_default();
        Ok(songs)
    }

    pub fn all_songs(&self) -> Result<Vec<Song>> {
        let mut offset = 0;
        let page_size = 200;
        let mut album_ids = Vec::new();
        loop {
            let json = self.request(
                "getAlbumList2",
                &[
                    ("type", "alphabeticalByName".to_string()),
                    ("size", page_size.to_string()),
                    ("offset", offset.to_string()),
                ],
            )?;
            let albums = json
                .get("subsonic-response")
                .and_then(|v| v.get("albumList2"))
                .and_then(|v| v.get("album"))
                .map(parse_album_ids)
                .unwrap_or_default();
            if albums.is_empty() {
                break;
            }
            album_ids.extend(albums);
            offset += page_size;
        }

        let mut songs = Vec::new();
        for album_id in album_ids {
            let mut album_songs = self.album_songs(&album_id)?;
            songs.append(&mut album_songs);
        }
        Ok(songs)
    }

    pub fn find_artist(&self, query: &str) -> Result<Option<Item>> {
        let json = self.request("search3", &[("query", query.to_string())])?;
        let items = json
            .get("subsonic-response")
            .and_then(|v| v.get("searchResult3"))
            .and_then(|v| v.get("artist"))
            .map(parse_items)
            .unwrap_or_default();
        Ok(best_match(query, &items))
    }

    pub fn find_album(&self, query: &str) -> Result<Option<Item>> {
        let json = self.request("search3", &[("query", query.to_string())])?;
        let items = json
            .get("subsonic-response")
            .and_then(|v| v.get("searchResult3"))
            .and_then(|v| v.get("album"))
            .map(parse_items)
            .unwrap_or_default();
        Ok(best_match(query, &items))
    }

    pub fn list_playlists(&self) -> Result<Vec<Item>> {
        let json = self.request("getPlaylists", &[])?;
        let items = json
            .get("subsonic-response")
            .and_then(|v| v.get("playlists"))
            .and_then(|v| v.get("playlist"))
            .map(parse_items)
            .unwrap_or_default();
        Ok(items)
    }

    pub fn find_playlist(&self, query: &str) -> Result<Option<Item>> {
        let items = self.list_playlists()?;
        Ok(best_match(query, &items))
    }

    pub fn artist_album_ids(&self, artist_id: &str) -> Result<Vec<String>> {
        let json = self.request("getArtist", &[("id", artist_id.to_string())])?;
        let albums = json
            .get("subsonic-response")
            .and_then(|v| v.get("artist"))
            .and_then(|v| v.get("album"))
            .map(parse_album_ids)
            .unwrap_or_default();
        Ok(albums)
    }

    pub fn album_songs(&self, album_id: &str) -> Result<Vec<Song>> {
        let json = self.request("getAlbum", &[("id", album_id.to_string())])?;
        let songs = json
            .get("subsonic-response")
            .and_then(|v| v.get("album"))
            .and_then(|v| v.get("song"))
            .map(parse_song_list)
            .unwrap_or_default();
        Ok(songs)
    }

    pub fn playlist_songs(&self, playlist_id: &str) -> Result<Vec<Song>> {
        let json = self.request("getPlaylist", &[("id", playlist_id.to_string())])?;
        let songs = json
            .get("subsonic-response")
            .and_then(|v| v.get("playlist"))
            .and_then(|v| v.get("entry"))
            .map(parse_song_list)
            .unwrap_or_default();
        Ok(songs)
    }

    pub fn scrobble_now_playing(&self, song_id: &str) -> Result<()> {
        let _ = self.request(
            "scrobble",
            &[
                ("id", song_id.to_string()),
                ("submission", "false".to_string()),
            ],
        )?;
        Ok(())
    }

    pub fn scrobble_submission(&self, song_id: &str) -> Result<()> {
        let _ = self.request(
            "scrobble",
            &[
                ("id", song_id.to_string()),
                ("submission", "true".to_string()),
            ],
        )?;
        Ok(())
    }

    pub fn set_rating(&self, song_id: &str, rating: u8) -> Result<()> {
        let _ = self.request(
            "setRating",
            &[("id", song_id.to_string()), ("rating", rating.to_string())],
        )?;
        Ok(())
    }

    pub fn star_song(&self, song_id: &str) -> Result<()> {
        let _ = self.request("star", &[("id", song_id.to_string())])?;
        Ok(())
    }

    pub fn unstar_song(&self, song_id: &str) -> Result<()> {
        let _ = self.request("unstar", &[("id", song_id.to_string())])?;
        Ok(())
    }

    pub fn create_playlist_with_song(&self, name: &str, song_id: &str) -> Result<()> {
        let _ = self.request(
            "createPlaylist",
            &[("name", name.to_string()), ("songId", song_id.to_string())],
        )?;
        Ok(())
    }

    pub fn add_song_to_playlist(&self, playlist_id: &str, song_id: &str) -> Result<()> {
        let _ = self.request(
            "updatePlaylist",
            &[
                ("playlistId", playlist_id.to_string()),
                ("songIdToAdd", song_id.to_string()),
            ],
        )?;
        Ok(())
    }

    pub fn delete_playlist(&self, playlist_id: &str) -> Result<()> {
        let _ = self.request(
            "deletePlaylist",
            &[("playlistId", playlist_id.to_string())],
        )?;
        Ok(())
    }

    pub fn starred_songs(&self) -> Result<Vec<Song>> {
        let json = self.request("getStarred2", &[])?;
        let songs = json
            .get("subsonic-response")
            .and_then(|v| v.get("starred2"))
            .and_then(|v| v.get("song"))
            .map(parse_song_list)
            .unwrap_or_default();
        Ok(songs)
    }

    fn token_pair(&self) -> (String, String) {
        let salt: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(8)
            .map(char::from)
            .collect();
        let token = format!("{:x}", md5::compute(format!("{}{}", self.password, salt)));
        (token, salt)
    }
}

fn parse_song_list(value: &Value) -> Vec<Song> {
    match value {
        Value::Array(items) => items.iter().filter_map(parse_song).collect(),
        Value::Object(_) => parse_song(value).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn parse_song(value: &Value) -> Option<Song> {
    let id = value.get("id")?.as_str()?.to_string();
    let title = value
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown Title")
        .to_string();
    let artist = value
        .get("artist")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown Artist")
        .to_string();
    let album = value
        .get("album")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown Album")
        .to_string();
    let duration = value.get("duration").and_then(|v| v.as_u64()).map(|v| v as u32);
    let track = value.get("track").and_then(|v| v.as_u64()).map(|v| v as u32);
    let disc = value
        .get("discNumber")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);

    Some(Song {
        id,
        title,
        artist,
        album,
        duration,
        track,
        disc,
    })
}

fn parse_album_ids(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(|v| v.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()))
            .collect(),
        Value::Object(_) => value
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn parse_items(value: &Value) -> Vec<Item> {
    match value {
        Value::Array(items) => items.iter().filter_map(parse_item).collect(),
        Value::Object(_) => parse_item(value).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn parse_item(value: &Value) -> Option<Item> {
    let id = value.get("id")?.as_str()?.to_string();
    let name = value
        .get("name")
        .or_else(|| value.get("title"))
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();
    Some(Item { id, name })
}

fn normalize_name(input: &str) -> String {
    input
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn match_score(query: &str, candidate: &str) -> i32 {
    if candidate == query {
        3
    } else if candidate.contains(query) || query.contains(candidate) {
        2
    } else if candidate.starts_with(query) {
        1
    } else {
        0
    }
}

fn best_match(query: &str, items: &[Item]) -> Option<Item> {
    let normalized_query = normalize_name(query);
    if normalized_query.is_empty() {
        return None;
    }
    let mut best: Option<&Item> = None;
    let mut best_score = 0;
    for item in items {
        let normalized = normalize_name(&item.name);
        let score = match_score(&normalized_query, &normalized);
        if score > best_score {
            best_score = score;
            best = Some(item);
        }
    }
    best.map(|item| item.clone())
}
