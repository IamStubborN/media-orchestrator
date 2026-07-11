use std::{
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use url::Url;

pub fn spawn_truncated_http_response(status: &str, headers: &[&str]) -> (Url, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let headers = if headers.is_empty() {
        String::new()
    } else {
        format!("{}\r\n", headers.join("\r\n"))
    };
    let response = format!(
        "HTTP/1.1 {status}\r\n{headers}Connection: close\r\nContent-Length: 64\r\n\r\nraw-body-secret"
    );

    let server = thread::spawn(move || {
        let mut stream = accept_with_deadline(&listener);
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request);
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.shutdown(Shutdown::Both);
    });

    (origin, server)
}

pub fn spawn_http_body_response(
    headers: &[&str],
    body_len: usize,
    chunked: bool,
) -> (Url, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let headers = if headers.is_empty() {
        String::new()
    } else {
        format!("{}\r\n", headers.join("\r\n"))
    };

    let server = thread::spawn(move || {
        let mut stream = accept_with_deadline(&listener);
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request);

        if chunked {
            let head = format!(
                "HTTP/1.1 200 OK\r\n{headers}Connection: close\r\nTransfer-Encoding: chunked\r\n\r\n{body_len:X}\r\n"
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&vec![b'x'; body_len]);
            let _ = stream.write_all(b"\r\n0\r\n\r\n");
        } else {
            let head = format!(
                "HTTP/1.1 200 OK\r\n{headers}Connection: close\r\nContent-Length: {body_len}\r\n\r\n"
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&vec![b'x'; body_len]);
        }
        let _ = stream.shutdown(Shutdown::Both);
    });

    (origin, server)
}

pub fn spawn_declared_http_body_response(body_len: usize) -> (Url, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();

    let server = thread::spawn(move || {
        let mut stream = accept_with_deadline(&listener);
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request);
        let response =
            format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {body_len}\r\n\r\n");
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.shutdown(Shutdown::Both);
    });

    (origin, server)
}

fn accept_with_deadline(listener: &TcpListener) -> TcpStream {
    const IO_TIMEOUT: Duration = Duration::from_secs(2);
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + IO_TIMEOUT;

    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "raw HTTP client did not connect");
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("raw HTTP accept failed: {error}"),
        }
    };
    stream.set_nonblocking(false).unwrap();
    stream.set_read_timeout(Some(IO_TIMEOUT)).unwrap();
    stream.set_write_timeout(Some(IO_TIMEOUT)).unwrap();
    stream
}
