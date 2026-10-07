//! `aionix-healthcheck <url> [--print]` — tiny HTTP probe for container health checks
//! (the runtime image has no shell, curl or wget). Exits 0 on HTTP 200.
//! Only plain `http://host:port/path` URLs are supported (container-local probes).

use std::{
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    process::ExitCode,
    time::Duration,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let print = args.iter().any(|a| a == "--print");
    let Some(url) = args.iter().find(|a| a.starts_with("http://")) else {
        eprintln!("usage: aionix-healthcheck http://127.0.0.1:8080/readyz [--print]");
        return ExitCode::from(2);
    };
    let rest = &url["http://".len()..];
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let Some(addr) = hostport.to_socket_addrs().ok().and_then(|mut a| a.next()) else {
        eprintln!("cannot resolve {hostport}");
        return ExitCode::from(1);
    };
    let timeout = Duration::from_secs(5);
    let mut stream = match TcpStream::connect_timeout(&addr, timeout) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("connect {addr}: {e}");
            return ExitCode::from(1);
        }
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {hostport}\r\nConnection: close\r\nUser-Agent: aionix-healthcheck\r\n\r\n"
    );
    if stream.write_all(req.as_bytes()).is_err() {
        return ExitCode::from(1);
    }
    let mut buf = Vec::new();
    let _ = stream.take(1 << 20).read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    let ok = text
        .lines()
        .next()
        .is_some_and(|status| status.split_whitespace().nth(1) == Some("200"));
    if print && let Some(i) = text.find("\r\n\r\n") {
        let body = &text[i + 4..];
        // Chunked bodies: print as-is minus chunk-size lines (JSON contains no bare hex lines).
        for line in body.lines() {
            if !line.chars().all(|c| c.is_ascii_hexdigit()) || line.is_empty() {
                println!("{line}");
            }
        }
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
