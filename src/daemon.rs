use crate::config::Config;
use crate::player::{MpvController, MpvEvent};
use crate::protocol::{Response, SongInfo, Status};
use crate::subsonic::{NavidromeClient, Song};
use anyhow::{anyhow, Context, Result};
use rand::seq::SliceRandom;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

struct State {
    queue: Vec<Song>,
    index: usize,
    current: Option<Song>,
    paused: bool,
    repeat: bool,
    shuffle: bool,
    suppress_next_end: bool,
    end_grace_ms: u64,
}

impl State {
    fn new(end_grace_ms: u64) -> Self {
        Self {
            queue: Vec::new(),
            index: 0,
            current: None,
            paused: false,
            repeat: false,
            shuffle: false,
            suppress_next_end: false,
            end_grace_ms,
        }
    }

    fn status(&self) -> Status {
        Status {
            song: self.current.as_ref().map(|song| SongInfo {
                id: song.id.clone(),
                title: song.title.clone(),
                artist: song.artist.clone(),
                album: song.album.clone(),
            }),
            paused: self.paused,
            queue_len: self.queue.len(),
            index: self.index,
        }
    }
}

pub fn run(config: Config) -> Result<()> {
    let socket_path = Config::socket_path()?;
    if socket_path.exists() {
        fs::remove_file(&socket_path).ok();
    }

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("Failed to bind socket {}", socket_path.display()))?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;

    let mpv_socket = Config::mpv_socket_path()?;
    let mpv = match MpvController::spawn(&mpv_socket) {
        Ok(mpv) => Arc::new(mpv),
        Err(err) => {
            fs::remove_file(&socket_path).ok();
            return Err(err);
        }
    };

    let client = NavidromeClient::new(&config)?;
    let state = Arc::new(Mutex::new(State::new(config.end_grace_ms())));

    let (event_tx, event_rx) = mpsc::channel();
    mpv.start_event_loop(event_tx)?;

    start_event_handler(state.clone(), client.clone(), mpv.clone(), event_rx);

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let state = state.clone();
                let client = client.clone();
                let mpv = mpv.clone();
                let config = config.clone();
                thread::spawn(move || {
                    if let Err(err) = handle_connection(stream, state, client, mpv, config) {
                        eprintln!("simplay: error handling client: {}", err);
                    }
                });
            }
            Err(err) => eprintln!("simplay: socket accept error: {}", err),
        }
    }

    Ok(())
}

fn start_event_handler(
    state: Arc<Mutex<State>>,
    client: NavidromeClient,
    mpv: Arc<MpvController>,
    event_rx: mpsc::Receiver<MpvEvent>,
) {
    thread::spawn(move || {
        while let Ok(event) = event_rx.recv() {
            match event {
                MpvEvent::EndFile { reason } => {
                    let reason = reason.unwrap_or_default();
                    let suppress = {
                        if let Ok(mut st) = state.lock() {
                            if st.suppress_next_end {
                                st.suppress_next_end = false;
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    };
                    if suppress {
                        continue;
                    }
                    let should_advance =
                        reason.is_empty() || matches!(reason.as_str(), "eof" | "stop" | "error");
                    if reason == "eof" {
                        let ended = { state.lock().ok().and_then(|s| s.current.clone()) };
                        if let Some(song) = ended {
                            let client = client.clone();
                            let song_id = song.id.clone();
                            thread::spawn(move || {
                                if let Err(err) = client.scrobble_submission(&song_id) {
                                    eprintln!("simplay: scrobble failed: {}", err);
                                }
                            });
                        }
                    }
                    if should_advance {
                        if let Err(err) = play_next(&state, &client, &mpv, false, None) {
                            eprintln!("simplay: next track failed: {}", err);
                        }
                    }
                }
            }
        }
    });
}

fn handle_connection(
    stream: UnixStream,
    state: Arc<Mutex<State>>,
    client: NavidromeClient,
    mpv: Arc<MpvController>,
    config: Config,
) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;

    let req: crate::protocol::Request = serde_json::from_str(&line)?;
    let response = handle_command(req, &state, &client, &mpv, &config);

    let mut writer = BufWriter::new(stream);
    serde_json::to_writer(&mut writer, &response)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn handle_command(
    req: crate::protocol::Request,
    state: &Arc<Mutex<State>>,
    client: &NavidromeClient,
    mpv: &Arc<MpvController>,
    config: &Config,
) -> Response {
    match req.cmd.as_str() {
        "shuffle" => match shuffle_library(client, config) {
            Ok(mut songs) => {
                if songs.is_empty() {
                    return Response::err("No songs found");
                }
                if config.max_shuffle() > 0 && songs.len() > config.max_shuffle() {
                    songs.shuffle(&mut rand::thread_rng());
                    songs.truncate(config.max_shuffle());
                }
                songs.shuffle(&mut rand::thread_rng());
                if let Err(err) = set_queue_and_play(state, client, mpv, songs, true, true) {
                    return Response::err(err.to_string());
                }
                Response::ok("Shuffling library")
            }
            Err(err) => Response::err(err.to_string()),
        },
        "shuffleartist" => {
            let name = match req.arg {
                Some(arg) if !arg.trim().is_empty() => arg,
                _ => return Response::err("Artist name required"),
            };
            match shuffle_artist(client, &name) {
                Ok(mut songs) => {
                    if songs.is_empty() {
                        return Response::err("No songs found for artist");
                    }
                    songs.shuffle(&mut rand::thread_rng());
                    if let Err(err) = set_queue_and_play(state, client, mpv, songs, true, true) {
                        return Response::err(err.to_string());
                    }
                    Response::ok("Shuffling artist")
                }
                Err(err) => Response::err(err.to_string()),
            }
        }
        "shufflealbum" => {
            let name = match req.arg {
                Some(arg) if !arg.trim().is_empty() => arg,
                _ => return Response::err("Album name required"),
            };
            match client.find_album(&name) {
                Ok(Some(album)) => match client.album_songs(&album.id) {
                    Ok(mut songs) => {
                        if songs.is_empty() {
                            return Response::err("No songs found for album");
                        }
                        songs.shuffle(&mut rand::thread_rng());
                        if let Err(err) = set_queue_and_play(state, client, mpv, songs, true, true) {
                            return Response::err(err.to_string());
                        }
                        Response::ok(format!("Shuffling album {}", album.name))
                    }
                    Err(err) => Response::err(err.to_string()),
                },
                Ok(None) => Response::err("Album not found"),
                Err(err) => Response::err(err.to_string()),
            }
        }
        "shuffleplaylist" => {
            let name = match req.arg {
                Some(arg) if !arg.trim().is_empty() => arg,
                _ => return Response::err("Playlist name required"),
            };
            match client.find_playlist(&name) {
                Ok(Some(list)) => match client.playlist_songs(&list.id) {
                    Ok(mut songs) => {
                        if songs.is_empty() {
                            return Response::err("No songs found for playlist");
                        }
                        songs.shuffle(&mut rand::thread_rng());
                        if let Err(err) = set_queue_and_play(state, client, mpv, songs, true, true) {
                            return Response::err(err.to_string());
                        }
                        Response::ok(format!("Shuffling playlist {}", list.name))
                    }
                    Err(err) => Response::err(err.to_string()),
                },
                Ok(None) => Response::err("Playlist not found"),
                Err(err) => Response::err(err.to_string()),
            }
        }
        "playalbum" => {
            let name = match req.arg {
                Some(arg) if !arg.trim().is_empty() => arg,
                _ => return Response::err("Album name required"),
            };
            match client.find_album(&name) {
                Ok(Some(album)) => match client.album_songs(&album.id) {
                    Ok(mut songs) => {
                        if songs.is_empty() {
                            return Response::err("No songs found for album");
                        }
                        songs.sort_by_key(|song| (song.disc.unwrap_or(0), song.track.unwrap_or(0)));
                        if let Err(err) = set_queue_and_play(state, client, mpv, songs, false, false) {
                            return Response::err(err.to_string());
                        }
                        Response::ok(format!("Playing album {}", album.name))
                    }
                    Err(err) => Response::err(err.to_string()),
                },
                Ok(None) => Response::err("Album not found"),
                Err(err) => Response::err(err.to_string()),
            }
        }
        "fastforward" => match play_next(state, client, mpv, true, None) {
            Ok(_) => Response::ok("Next track"),
            Err(err) => Response::err(err.to_string()),
        },
        "rewind" => match play_previous(state, client, mpv, true) {
            Ok(_) => Response::ok("Previous track"),
            Err(err) => Response::err(err.to_string()),
        },
        "pause" => match mpv.pause(true) {
            Ok(_) => {
                if let Ok(mut st) = state.lock() {
                    st.paused = true;
                }
                Response::ok("Paused")
            }
            Err(err) => Response::err(err.to_string()),
        },
        "play" => match mpv.pause(false) {
            Ok(_) => {
                if let Ok(mut st) = state.lock() {
                    st.paused = false;
                }
                Response::ok("Playing")
            }
            Err(err) => Response::err(err.to_string()),
        },
        "startover" => match mpv.seek_absolute(0.0) {
            Ok(_) => Response::ok("Restarted"),
            Err(err) => Response::err(err.to_string()),
        },
        "likesong" => match current_song(state) {
            Some(song) => match client.star_song(&song.id) {
                Ok(_) => Response::ok("Hearted song"),
                Err(err) => Response::err(err.to_string()),
            },
            None => Response::err("No song playing"),
        },
        "unlikesong" => match current_song(state) {
            Some(song) => match client.unstar_song(&song.id) {
                Ok(_) => Response::ok("Unhearted song"),
                Err(err) => Response::err(err.to_string()),
            },
            None => Response::err("No song playing"),
        },
        "rate" => {
            let rating = match req.arg {
                Some(arg) => match arg.trim().parse::<u8>() {
                    Ok(value) if (1..=5).contains(&value) => value,
                    _ => return Response::err("Rating must be 1-5"),
                },
                None => return Response::err("Rating required"),
            };
            match current_song(state) {
                Some(song) => match client.set_rating(&song.id, rating) {
                    Ok(_) => Response::ok(format!("Rated song {}", rating)),
                    Err(err) => Response::err(err.to_string()),
                },
                None => Response::err("No song playing"),
            }
        }
        "shuffleliked" => match client.starred_songs() {
            Ok(mut songs) => {
                if songs.is_empty() {
                    return Response::err("No liked songs found");
                }
                if config.max_shuffle() > 0 && songs.len() > config.max_shuffle() {
                    songs.shuffle(&mut rand::thread_rng());
                    songs.truncate(config.max_shuffle());
                }
                songs.shuffle(&mut rand::thread_rng());
                if let Err(err) = set_queue_and_play(state, client, mpv, songs, true, true) {
                    return Response::err(err.to_string());
                }
                Response::ok("Shuffling liked songs")
            }
            Err(err) => Response::err(err.to_string()),
        },
        "volumeup" => adjust_volume(mpv, config.volume_step() as i32),
        "volumedown" => adjust_volume(mpv, -(config.volume_step() as i32)),
        "addsongtoplaylist" => {
            let playlist_name = match req.arg {
                Some(arg) if !arg.trim().is_empty() => arg,
                _ => return Response::err("Playlist name required"),
            };
            let song = match current_song(state) {
                Some(song) => song,
                None => return Response::err("No song playing"),
            };
            match client.find_playlist(&playlist_name) {
                Ok(Some(playlist)) => match client.add_song_to_playlist(&playlist.id, &song.id) {
                    Ok(_) => Response::ok(format!("Added to playlist {}", playlist.name)),
                    Err(err) => Response::err(err.to_string()),
                },
                Ok(None) => match client.create_playlist_with_song(&playlist_name, &song.id) {
                    Ok(_) => Response::ok(format!("Created playlist {}", playlist_name)),
                    Err(err) => Response::err(err.to_string()),
                },
                Err(err) => Response::err(err.to_string()),
            }
        }
        "deleteplaylist" => {
            let playlist_name = match req.arg {
                Some(arg) if !arg.trim().is_empty() => arg,
                _ => return Response::err("Playlist name required"),
            };
            match client.find_playlist(&playlist_name) {
                Ok(Some(playlist)) => match client.delete_playlist(&playlist.id) {
                    Ok(_) => Response::ok(format!("Deleted playlist {}", playlist.name)),
                    Err(err) => Response::err(err.to_string()),
                },
                Ok(None) => Response::err("Playlist not found"),
                Err(err) => Response::err(err.to_string()),
            }
        }
        "status" => {
            let status = state.lock().map(|s| s.status()).unwrap_or(Status {
                song: None,
                paused: false,
                queue_len: 0,
                index: 0,
            });
            Response {
                ok: true,
                message: "ok".to_string(),
                status: Some(status),
            }
        }
        _ => Response::err("Unknown command"),
    }
}

fn shuffle_library(client: &NavidromeClient, config: &Config) -> Result<Vec<Song>> {
    if config.max_shuffle() == 0 {
        client.all_songs()
    } else {
        client.get_random_songs(config.max_shuffle())
    }
}

fn shuffle_artist(client: &NavidromeClient, query: &str) -> Result<Vec<Song>> {
    let artist = client
        .find_artist(query)?
        .ok_or_else(|| anyhow!("Artist not found"))?;
    let album_ids = client.artist_album_ids(&artist.id)?;
    let mut songs = Vec::new();
    for album_id in album_ids {
        let mut album_songs = client.album_songs(&album_id)?;
        songs.append(&mut album_songs);
    }
    Ok(songs)
}

fn set_queue_and_play(
    state: &Arc<Mutex<State>>,
    client: &NavidromeClient,
    mpv: &Arc<MpvController>,
    songs: Vec<Song>,
    repeat: bool,
    shuffle: bool,
) -> Result<()> {
    if songs.is_empty() {
        return Err(anyhow!("No songs to play"));
    }
    let first = songs[0].clone();
    {
        let mut st = state.lock().map_err(|_| anyhow!("State lock poisoned"))?;
        st.queue = songs;
        st.index = 0;
        st.current = Some(first.clone());
        st.paused = false;
        st.repeat = repeat;
        st.shuffle = shuffle;
        st.suppress_next_end = false;
    }
    play_song(state, client, mpv, &first)?;
    Ok(())
}

fn play_next(
    state: &Arc<Mutex<State>>,
    client: &NavidromeClient,
    mpv: &Arc<MpvController>,
    manual: bool,
    expected_id: Option<&str>,
) -> Result<()> {
    let next = {
        let mut st = state.lock().map_err(|_| anyhow!("State lock poisoned"))?;
        if st.queue.is_empty() {
            return Err(anyhow!("Queue is empty"));
        }
        if let Some(expected) = expected_id {
            if st
                .current
                .as_ref()
                .map(|song| song.id.as_str() == expected)
                != Some(true)
            {
                return Ok(());
            }
        }
        if st.index + 1 >= st.queue.len() {
            if st.repeat {
                if st.shuffle {
                    st.queue.shuffle(&mut rand::thread_rng());
                }
                st.index = 0;
            } else {
                return Err(anyhow!("End of queue"));
            }
        } else {
            st.index += 1;
        }
        let song = st.queue[st.index].clone();
        st.current = Some(song.clone());
        st.paused = false;
        if manual {
            st.suppress_next_end = true;
        }
        song
    };
    play_song(state, client, mpv, &next)?;
    Ok(())
}

fn play_previous(
    state: &Arc<Mutex<State>>,
    client: &NavidromeClient,
    mpv: &Arc<MpvController>,
    manual: bool,
) -> Result<()> {
    let prev = {
        let mut st = state.lock().map_err(|_| anyhow!("State lock poisoned"))?;
        if st.queue.is_empty() {
            return Err(anyhow!("Queue is empty"));
        }
        if st.index == 0 {
            return Err(anyhow!("At start of queue"));
        }
        st.index -= 1;
        let song = st.queue[st.index].clone();
        st.current = Some(song.clone());
        st.paused = false;
        if manual {
            st.suppress_next_end = true;
        }
        song
    };
    play_song(state, client, mpv, &prev)?;
    Ok(())
}

fn play_song(
    state: &Arc<Mutex<State>>,
    client: &NavidromeClient,
    mpv: &Arc<MpvController>,
    song: &Song,
) -> Result<()> {
    let url = client.stream_url(&song.id)?;
    mpv.load(&url)?;
    mpv.pause(false)?;
    if let Err(err) = client.scrobble_now_playing(&song.id) {
        eprintln!("simplay: now playing update failed: {}", err);
    }
    if let Some(duration) = song.duration {
        schedule_end_fallback(
            state.clone(),
            client.clone(),
            mpv.clone(),
            song.id.clone(),
            duration,
        );
    }
    Ok(())
}

fn schedule_end_fallback(
    state: Arc<Mutex<State>>,
    client: NavidromeClient,
    mpv: Arc<MpvController>,
    song_id: String,
    duration_secs: u32,
) {
    thread::spawn(move || {
        let mut remaining = duration_secs as f64;
        loop {
            let grace_ms = state
                .lock()
                .ok()
                .map(|s| s.end_grace_ms)
                .unwrap_or(500);
            let sleep_ms = ((remaining * 1000.0) as u64).saturating_add(grace_ms);
            thread::sleep(Duration::from_millis(sleep_ms));

            let (paused, current_matches) = match state.lock() {
                Ok(st) => (
                    st.paused,
                    st.current.as_ref().map(|song| song.id == song_id).unwrap_or(false),
                ),
                Err(_) => (true, false),
            };
            if !current_matches || paused {
                return;
            }

            if let Ok(Some(pos)) = mpv.get_time_pos() {
                if pos + 0.25 < duration_secs as f64 {
                    remaining = (duration_secs as f64 - pos).max(0.1);
                    continue;
                }
            }

            if let Err(err) = play_next(&state, &client, &mpv, true, Some(song_id.as_str())) {
                eprintln!("simplay: fallback next track failed: {}", err);
            }
            return;
        }
    });
}

fn current_song(state: &Arc<Mutex<State>>) -> Option<Song> {
    state.lock().ok().and_then(|s| s.current.clone())
}

fn adjust_volume(mpv: &Arc<MpvController>, delta: i32) -> Response {
    match mpv.get_volume() {
        Ok(volume) => {
            let new_volume = (volume as i32 + delta).clamp(0, 100) as f64;
            match mpv.set_volume(new_volume) {
                Ok(_) => Response::ok(format!("Volume {}", new_volume as i32)),
                Err(err) => Response::err(err.to_string()),
            }
        }
        Err(err) => Response::err(err.to_string()),
    }
}
