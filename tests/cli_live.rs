use std::io::Write;
use std::process::{Command, Stdio};

fn test_url() -> String {
    // The live test intentionally uses a real URL to exercise the complete CLI flow.
    dotenvy::dotenv().ok();
    std::env::var("YOUTUBE_TEST_URL").expect("YOUTUBE_TEST_URL must be set in .env")
}

#[test]
fn running_cli_reports_invalid_url_from_stdin() {
    // Invalid input should be rejected before the CLI tries to query a provider.
    std::fs::create_dir_all("target/live-cli-downloads").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_downloader"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("INSTAGRAM_COOKIES_FILE", "cookies/instagram-cookies.txt")
        .env("YOUTUBE_COOKIES_FILE", "cookies/youtube-cookies.txt")
        .env("DOWNLOADS_DIRECTORY", "target/live-cli-downloads")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("CLI should start");

    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"not a url\n")
        .unwrap();
    let output = child.wait_with_output().expect("CLI should finish");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("URL:"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Invalid URL"));
}

#[test]
fn running_cli_fetches_media_info_and_downloads_a_selected_file() {
    // Feed the URL and a format choice through stdin, just as an interactive user would.
    std::fs::create_dir_all("target/live-cli-downloads").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_downloader"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("INSTAGRAM_COOKIES_FILE", "cookies/instagram-cookies.txt")
        .env("YOUTUBE_COOKIES_FILE", "cookies/youtube-cookies.txt")
        .env("DOWNLOADS_DIRECTORY", "target/live-cli-downloads")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("CLI should start");

    let input = format!("{}\n1\n", test_url());
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().expect("CLI should finish");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("URL:"));
    // The CLI displays media information as selectable formats before downloading.
    assert!(stdout.contains("Select format:"));
    assert!(stdout.contains("Download complete!"));
}
