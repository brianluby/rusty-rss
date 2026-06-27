//! Shared loopback HTTP servers for the capture tests.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) fn serve_html(body: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("server should bind");
    let addr = listener.local_addr().expect("local address should exist");
    let body = body.to_string();

    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("server should accept request");
        let mut request = [0u8; 4096];
        let _ = stream.read(&mut request).expect("request should read");
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("response should write");
    });

    format!("http://{addr}/article")
}

pub(crate) fn serve_status_sequence(statuses: Vec<&'static str>, body: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("server should bind");
    let addr = listener.local_addr().expect("local address should exist");
    let body = body.to_string();

    std::thread::spawn(move || {
        for status in statuses {
            let (mut stream, _) = listener.accept().expect("server should accept request");
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request).expect("request should read");
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("response should write");
        }
    });

    format!("http://{addr}/article")
}

pub(crate) fn serve_concurrent_html(
    response_count: usize,
    current: Arc<AtomicUsize>,
    max_seen: Arc<AtomicUsize>,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("server should bind");
    let addr = listener.local_addr().expect("local address should exist");

    std::thread::spawn(move || {
        for _ in 0..response_count {
            let (mut stream, _) = listener.accept().expect("server should accept request");
            let current = Arc::clone(&current);
            let max_seen = Arc::clone(&max_seen);
            std::thread::spawn(move || {
                let now = current.fetch_add(1, Ordering::SeqCst) + 1;
                max_seen.fetch_max(now, Ordering::SeqCst);
                let mut request = [0u8; 4096];
                let _ = stream.read(&mut request).expect("request should read");
                std::thread::sleep(std::time::Duration::from_millis(80));
                let body = "<html><head><title>Concurrent page</title></head><body>Concurrent body</body></html>";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("response should write");
                current.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });

    format!("http://{addr}")
}

pub(crate) fn serve_oversized_no_length() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("server should bind");
    let addr = listener.local_addr().expect("local address should exist");

    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("server should accept request");
        let mut request = [0u8; 4096];
        let _ = stream.read(&mut request);
        // No Content-Length: the body is close-delimited, so the size cap can
        // only be enforced by reading incrementally.
        let header = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n";
        if stream.write_all(header.as_bytes()).is_err() {
            return;
        }
        let chunk = vec![b'a'; 64 * 1024];
        // 2 MiB total, double the 1 MiB cap. Stop once the client disconnects.
        for _ in 0..32 {
            if stream.write_all(&chunk).is_err() {
                break;
            }
        }
    });

    format!("http://{addr}/big")
}
