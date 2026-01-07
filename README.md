# simplay

Headless Navidrome/Subsonic backend player with a CLI controller. It streams audio to the default device via `mpv`, updates Navidrome "Now Playing", and exposes a local socket for fast control (suitable for a plasmoid to call).

## Requirements
- Rust (1.70+ recommended)
- `mpv` installed and in PATH
  - If `mpv` is installed elsewhere, set `SIMPLAY_MPV=/path/to/mpv`

## Build and install (local)
```bash
cargo build --release
```
Binary is at `target/release/simplay`.

## Install (user)
```bash
cargo install --path .
```

## First run / config
Run any command or explicitly configure:
```bash
simplay --configure
```
Config is stored at `~/.config/simplay/simplay.conf` and is created with `0600` permissions.

## Run the backend (foreground)
```bash
simplay --daemon
```

## Example CLI control
```bash
simplay --shuffle
simplay --pause
simplay --play
simplay --shuffleartist "Shawn James"
simplay --addsongtoplaylist "Roadtrip"
```

## Systemd user service
1) Copy the unit file:
```bash
mkdir -p ~/.config/systemd/user
cp systemd/simplay.service ~/.config/systemd/user/
```

2) Enable and start:
```bash
systemctl --user enable --now simplay
```

## Commands
Most commands have a short alias. Only one action is expected per invocation.

- `--shuffle`, `-s`
- `--pause`, `-p`
- `--play`, `-P`
- `--fastforward`, `-f`
- `--rewind`, `-r`
- `--startover`, `-o`
- `--likesong`, `-l` (heart song)
- `--unlikesong`, `-u` (unheart song)
- `--rate <1-5>`, `-R`
- `--volumeup`, `-v`
- `--volumedown`, `-V`
- `--shuffleliked`, `-H`
- `--shuffleartist <artist>`, `-a`
- `--shufflealbum <album>`, `-b`
- `--shuffleplaylist <playlist>`, `-g`
- `--playalbum <album>`, `-A`
- `--addsongtoplaylist <playlist>`, `-c`
- `--deleteplaylist <playlist>`, `-D`
- `--status`, `-t`
- `--api <endpoint> --param key=value` (pass-through to Subsonic)

## Notes
- No credentials or personal info are stored in this repo. Only the local config file is used.
- `--shuffle` loads the full library when `max_shuffle = 0` in config (default). Set `max_shuffle` to cap the shuffle size.
- `end_grace_ms` controls the fallback delay after a track ends before auto-advancing (default 500ms).
- If you change servers or want to tweak defaults (API version, TLS verify), edit the config or re-run `simplay --configure`.
