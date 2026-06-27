use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// Remove a stale test database and its SQLite sidecars so a reused temp path
/// never inherits data from an earlier run. Covers the default rollback-journal
/// mode (`-journal`) and WAL mode (`-wal`/`-shm`) so it stays correct regardless
/// of the connection's journal_mode. A missing file is fine; any other I/O error
/// is surfaced rather than silently ignored.
pub(crate) fn reset_db_file(path: &Path) {
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let mut target = path.as_os_str().to_owned();
        target.push(suffix);
        let target = PathBuf::from(target);
        match std::fs::remove_file(&target) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => panic!(
                "failed to remove stale test db file {}: {err}",
                target.display()
            ),
        }
    }
}

pub(crate) fn serve_json_responses(responses: Vec<String>) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("server should bind");
    let addr = listener.local_addr().expect("local address should exist");
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        for body in responses {
            let (mut stream, _) = listener.accept().expect("server should accept request");
            let request = read_http_request(&mut stream);
            tx.send(String::from_utf8_lossy(&request).to_string())
                .expect("request should be sent to test");

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("response should be written");
        }
    });

    (format!("http://{addr}/v1"), rx)
}

fn read_http_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut buffer = [0u8; 1024];

    loop {
        let read = stream
            .read(&mut buffer)
            .expect("request should be readable");
        if read == 0 {
            return request;
        }

        request.extend_from_slice(&buffer[..read]);

        if let Some(header_end) = find_header_end(&request) {
            let body_start = header_end + 4;
            let content_length = content_length(&request[..header_end]).unwrap_or(0);
            if request.len() >= body_start + content_length {
                return request;
            }
        }
    }
}

fn find_header_end(request: &[u8]) -> Option<usize> {
    request.windows(4).position(|window| window == b"\r\n\r\n")
}

fn content_length(headers: &[u8]) -> Option<usize> {
    let headers = String::from_utf8_lossy(headers);
    headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse().ok())?
    })
}
