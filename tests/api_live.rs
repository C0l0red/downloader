use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;
use std::process::{Command, Stdio};

fn test_url() -> String {
    // The live tests use a real URL so they exercise yt-dlp and the configured cookies.
    dotenvy::dotenv().ok();
    std::env::var("YOUTUBE_TEST_URL").expect("YOUTUBE_TEST_URL must be set in .env")
}

fn configured_command() -> Command {
    // Keep live-test downloads separate from normal application downloads.
    std::fs::create_dir_all("target/live-api-downloads").unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_downloader"));
    command
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("INSTAGRAM_COOKIES_FILE", "cookies/instagram-cookies.txt")
        .env("YOUTUBE_COOKIES_FILE", "cookies/youtube-cookies.txt")
        .env("DOWNLOADS_DIRECTORY", "target/live-api-downloads");
    command
}

fn available_address(addr: &str) -> String {
    let listener = std::net::TcpListener::bind(addr).unwrap();
    listener.local_addr().unwrap().to_string()
}

#[test]
fn running_server_serves_ui_through_http() {
    let address = available_address("127.0.0.1:39091");
    let mut server = configured_command()
        .args(["server", &address])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("server should start");

    // let address = "127.0.0.1:39091";
    let mut stream = None;
    // The server starts asynchronously, so wait until its TCP listener is ready.
    for _ in 0..50 {
        if let Ok(connection) = TcpStream::connect(&address) {
            stream = Some(connection);
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let mut stream = stream.expect("live API should accept connections");
    stream.write_all(b"GET /ui HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();

    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert!(response.contains("<!doctype html>") || response.contains("<!DOCTYPE html>"));
    assert!(response.contains("<h1>Downloader</h1>"));

    server.kill().expect("server should stop");
    let _ = server.wait();
}

#[test]
fn running_server_returns_media_info_and_downloads_a_selected_file() {
    let address = available_address("127.0.0.1:39092");
    let mut server = configured_command()
        .args(["server", &address])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("server should start");

    let mut ready = false;
    // Wait for the child process to bind before sending the media-info request.
    for _ in 0..50 {
        if TcpStream::connect(&address).is_ok() {
            ready = true;
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    if !ready {
        let output = server.wait_with_output().expect("server should finish");
        panic!(
            "live API should accept connections: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // The media info endpoint returns a vector of media information, including the formats that can
    // be selected for each media info if more than one is returned.
    let url = test_url();
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("url", &url)
        .finish();
    let media_info_response = String::from_utf8(request(&address, &format!("GET /media-info?{query} HTTP/1.1")))
    .unwrap();
    assert!(media_info_response.starts_with("HTTP/1.1 200 OK"), "{media_info_response}");
    let media_info_body = media_info_response
        .split_once("\r\n\r\n")
        .expect("media-info response should have a body")
        .1;
    let media_items: serde_json::Value = serde_json::from_str(media_info_body).unwrap();
    let formats = media_items[0]["formats"]
        .as_array()
        .expect("media info should include selectable formats");
    assert!(!formats.is_empty(), "the live URL should expose at least one format");
    assert!(formats[0]["format_id"].as_str().is_some());

    // Use the selected media item and format to verify the API returns the downloaded file.

    let download_query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("url", &url)
        .append_pair("item_index", "0")
        .append_pair("download_option_index", "1")
        .finish();
    let download_response = request(&address, &format!("GET /download?{download_query} HTTP/1.1"));
    let header_end = download_response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("download response should have headers")
        + 4;
    let headers = String::from_utf8(download_response[..header_end].to_vec()).unwrap();
    assert!(headers.starts_with("HTTP/1.1 200 OK"), "{headers}");
    assert!(headers.to_ascii_lowercase().contains("content-type:"));
    assert!(headers.to_ascii_lowercase().contains("content-disposition:"));
    let download_body = &download_response[header_end..];
    assert!(!download_body.is_empty(), "the downloaded file should not be empty");

    server.kill().expect("server should stop");
    let _ = server.wait();
}

fn request(address: &str, request: &str) -> Vec<u8> {
    // Send a minimal HTTP/1.1 request and collect headers plus the complete response body.
    let mut stream = TcpStream::connect(address).expect("live API should accept connections");
    stream
        .write_all(format!("{request}\r\nHost: localhost\r\nConnection: close\r\n\r\n").as_bytes())
        .unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    response
}
