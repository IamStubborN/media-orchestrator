use std::{
    io::{Read, Write},
    net::{Shutdown, TcpListener},
    thread::{self, JoinHandle},
    time::Duration,
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
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request);
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.shutdown(Shutdown::Both);
    });

    (origin, server)
}
