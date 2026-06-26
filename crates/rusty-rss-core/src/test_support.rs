use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

pub(crate) fn serve_json_responses(responses: Vec<String>) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("server should bind");
    let addr = listener.local_addr().expect("local address should exist");
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        for body in responses {
            let (mut stream, _) = listener.accept().expect("server should accept request");
            let mut request = [0u8; 8192];
            let read = stream
                .read(&mut request)
                .expect("request should be readable");
            tx.send(String::from_utf8_lossy(&request[..read]).to_string())
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
