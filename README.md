# downloader
This is a lightweight application that helps download videos
from websites like YouTube, Instagram, and more.  
I'm working on it for the sole purpose of having a personal downloader 
without having to go through the ad-ridden solutions available online.

It is still in development at the foundational level.

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

To start the Axum JSON API, use `server` and optionally provide a bind address:

```sh
cargo run -- server
cargo run -- server 0.0.0.0:3000
```

Open the browser interface at `/ui` to enter a URL, list its formats, and
start a download by selecting a format:

```text
http://localhost:3000/ui
```

List available formats with `GET /formats`:

```text
/formats?url=https%3A%2F%2Fwww.youtube.com%2Fwatch%3Fv%3D...
```

Download a format with `GET /download`:

```text
/download?url=https%3A%2F%2Fwww.youtube.com%2Fwatch%3Fv%3D...&format_id=401
```