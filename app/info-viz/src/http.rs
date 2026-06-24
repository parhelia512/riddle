use std::{
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
};

use crate::{app::AppState, web};

pub struct Server {
    addr: String,
    state: Arc<Mutex<AppState>>,
}

impl Server {
    pub fn new(addr: String, state: AppState) -> Self {
        Self {
            addr,
            state: Arc::new(Mutex::new(state)),
        }
    }

    pub fn serve(self) -> io::Result<()> {
        let listener = TcpListener::bind(&self.addr)?;
        println!("riddle-message-viz listening on http://{}", self.addr);

        for stream in listener.incoming() {
            match stream {
                Ok(mut stream) => {
                    if let Err(err) = handle_connection(&mut stream, &self.state) {
                        eprintln!("request failed: {err}");
                    }
                }
                Err(err) => eprintln!("connection failed: {err}"),
            }
        }

        Ok(())
    }
}

fn handle_connection(stream: &mut TcpStream, state: &Arc<Mutex<AppState>>) -> io::Result<()> {
    let request = read_request(stream)?;
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => {
            let source = state
                .lock()
                .expect("state mutex poisoned")
                .current_source()
                .to_string();
            respond(
                stream,
                200,
                "text/html; charset=utf-8",
                web::page_html(&source).as_bytes(),
            )
        }
        ("GET", "/source") => {
            let source = state
                .lock()
                .expect("state mutex poisoned")
                .current_source()
                .to_string();
            respond(stream, 200, "text/plain; charset=utf-8", source.as_bytes())
        }
        ("POST", "/graph") => {
            let source = String::from_utf8_lossy(&request.body).to_string();
            let html = state
                .lock()
                .expect("state mutex poisoned")
                .render_scope_graph(&source);
            respond(stream, 200, "text/html; charset=utf-8", html.as_bytes())
        }
        ("GET", "/favicon.ico") => respond(stream, 204, "text/plain", &[]),
        ("GET", path) if path.starts_with("/assets/") => match web::asset(path) {
            Some(asset) => respond(stream, 200, asset.content_type, asset.body),
            None => respond(stream, 404, "text/plain; charset=utf-8", b"not found"),
        },
        _ => respond(stream, 404, "text/plain; charset=utf-8", b"not found"),
    }
}

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> io::Result<Request> {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let header_end;

    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before headers",
            ));
        }
        data.extend_from_slice(&buf[..n]);
        if let Some(pos) = find_header_end(&data) {
            header_end = pos;
            break;
        }
    }

    let head = String::from_utf8_lossy(&data[..header_end]);
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap_or_default().to_string();
    let path = request_parts
        .next()
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_string();

    let mut content_len = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            content_len = value.trim().parse().unwrap_or(0);
        }
    }

    let body_start = header_end + 4;
    while data.len() < body_start + content_len {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
    }

    let available = data.len().saturating_sub(body_start);
    let body_len = content_len.min(available);
    let body = data[body_start..body_start + body_len].to_vec();
    Ok(Request { method, path, body })
}

fn find_header_end(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|w| w == b"\r\n\r\n")
}

fn respond(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        404 => "Not Found",
        _ => "OK",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)
}
