FROM rust:1.89-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install --no-install-recommends --yes ca-certificates curl ffmpeg python3 python3-pip unzip \
    && curl --fail --location --silent --show-error https://deno.land/install.sh | sh -s -- -y \
    && install -m 0755 /root/.deno/bin/deno /usr/local/bin/deno \
    && rm -rf /root/.deno \
    && pip3 install --break-system-packages --no-cache-dir yt-dlp \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --uid 10001 downloader \
    && mkdir -p /app/cookies /app/downloads \
    && touch /app/cookies/youtube-cookies.txt /app/cookies/instagram-cookies.txt \
    && chown -R downloader:downloader /app

COPY --from=builder /build/target/release/downloader /app/downloader

WORKDIR /app
ENV DOWNLOADS_DIRECTORY=/app/downloads \
    YOUTUBE_COOKIES_FILE=/app/cookies/youtube-cookies.txt \
    INSTAGRAM_COOKIES_FILE=/app/cookies/instagram-cookies.txt
EXPOSE 3424
USER downloader

ENTRYPOINT ["/app/downloader"]
CMD ["server", "0.0.0.0:3424"]