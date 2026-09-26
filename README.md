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

To start the Axum JSON API, use `server` and optionally provide a bind address:

```sh
cargo run -- server
cargo run -- server 0.0.0.0:3000
```

List available formats with `GET /formats`:

```text
/formats?url=https%3A%2F%2Fwww.youtube.com%2Fwatch%3Fv%3D...
```

Download a format with `GET /download`:

```text
/download?url=https%3A%2F%2Fwww.youtube.com%2Fwatch%3Fv%3D...&format_id=401
```