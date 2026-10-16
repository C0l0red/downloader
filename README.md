# Media Downloader
This is a lightweight application that helps download videos
from websites like YouTube, Instagram, and more.  
I'm working on it for the sole purpose of having a personal downloader 
without having to go through the ad-ridden solutions available online.

It is still in development at the foundational level.
Feel free to clone, fork or make contributions.

To use, it currently works by running the server locally and then making it accessible
from any device on your local network, including your phones, tablets, or other devices.  
You would need to find out your machine's local hostname, which is static and stays the same 
on any network it's connected to. It looks something like `my-macbook.local` or `my-pc.local`.

With that, all you need to do is run the server, using Rust or Docker, and then access it one of these ways:
1. The UI at `http://<your-machine's-local-hostname>:3424/ui`
2. The API at `http://<your-machine's-local-hostname>:3423/api` (pending when I add Swagger docs)
3. The CLI by entering `cargo run` in the terminal
4. My favorite way, by using [Siri Shortcuts](Siri%20Shortcut.md) (currently in Alpha)

## Running

The existing interactive CLI is the default:

```sh
cargo run
```

Live tests read their URLs from a local `.env` file. Copy `.env.example` to
`.env` and fill in the values before running the tests:

```sh
cp .env.example .env
cargo test
```

The `.env` file is ignored by Git so these values stay local.
Set `YOUTUBE_COOKIES_FILE` and `INSTAGRAM_COOKIES_FILE` in `.env` to the
cookie-file paths used by the corresponding downloaders. Set
`DOWNLOADS_DIRECTORY` to change where downloaded files are stored; it defaults
to the project's `downloads` directory.

To start the Axum JSON API, use `server` and optionally provide a bind address:

```sh
cargo run -- server
cargo run -- server 0.0.0.0:3424
```

Open the browser interface at `/ui` to enter a URL, list its formats, and
start a download by selecting a format:

```text
http://localhost:3424/ui
```

List available formats with `GET /formats`:

```text
/formats?url=https%3A%2F%2Fwww.youtube.com%2Fwatch%3Fv%3D...
```

Download a format with `GET /download`:

```text
/download?url=https%3A%2F%2Fwww.youtube.com%2Fwatch%3Fv%3D...&format_id=401
```

Downloads are returned to the client by default and removed from the server
after the response is opened. Add `keep_on_server=true` to keep the downloaded
file in the server's `downloads` directory instead:

```text
/download?url=https%3A%2F%2Fwww.youtube.com%2Fwatch%3Fv%3D...&format_id=401&keep_on_server=true
```

## Docker

Build the image from the project root:

```sh
docker build -t downloader .
```

The image includes the Rust application, `yt-dlp`, and `ffmpeg`. It starts the
JSON API on port `3424` by default. Create the cookie files before mounting
them; they may be empty for providers that do not require authentication:

```sh
mkdir -p cookies downloads
touch cookies/youtube-cookies.txt cookies/instagram-cookies.txt
docker run --rm \
  --publish 3424:3424 \
  --user "$(id -u):$(id -g)" \
  --volume "$PWD/cookies:/app/cookies:rw" \
  --volume "$PWD/downloads:/app/downloads:rw" \
  downloader
```

The same setup is available with Docker Compose. Create the cookie files and
directories first, then build, and start the service with your host user ID so
downloaded files remain writable on the host:

```sh
mkdir -p cookies downloads
touch cookies/youtube-cookies.txt cookies/instagram-cookies.txt
UID="$(id -u)" GID="$(id -g)" docker compose up --build
```

Stop it with `Ctrl-C`, or run `UID="$(id -u)" GID="$(id -g)" docker compose down`.

Open `http://localhost:3424/ui` after the container starts. The `cookies`
volume supplies the optional provider cookies, and the `downloads` volume
keeps files downloaded with `keep_on_server=true` after the container exits.
To run the interactive CLI instead, override the image command:

```sh
docker run --rm -it \
  --entrypoint /app/downloader \
  --user "$(id -u):$(id -g)" \
  --volume "$PWD/cookies:/app/cookies:rw" \
  --volume "$PWD/downloads:/app/downloads:rw" \
  downloader
```