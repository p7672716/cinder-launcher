//! A short-lived OAuth callback server bound to the loopback interface only.
//! Authorization codes and callback URLs must never enter logs.
use crate::microsoft_auth::{self, AuthRequest};
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::{Duration, Instant},
};

// OAuth is a user-facing action with an explicit end; do not keep a listener indefinitely.
const SIGN_IN_DEADLINE: Duration = Duration::from_secs(300);
const POLL: Duration = Duration::from_millis(40);

fn callback(
    stream: &mut TcpStream,
    request: &AuthRequest,
    port: u16,
) -> io::Result<Option<io::Result<String>>> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut first = String::new();
    BufReader::new(stream.try_clone()?.take(8192)).read_line(&mut first)?;
    let mut tokens = first.split_whitespace();
    let method = tokens.next();
    let path = tokens.next();
    let protocol = tokens.next();
    let (status, message, result) =
        if method != Some("GET") || !protocol.is_some_and(|p| p.starts_with("HTTP/1.")) {
            ("405 Method Not Allowed", "Invalid request", None)
        } else if !path.is_some_and(|p| p.starts_with("/callback?")) {
            ("404 Not Found", "Not found", None)
        } else {
            let target = format!("http://localhost:{port}{}", path.unwrap());
            match request.accept(&target) {
                Ok(code) => (
                    "200 OK",
                    "Sign-in response received. You may close this tab.",
                    Some(Ok(code)),
                ),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => (
                    "400 Bad Request",
                    "Sign-in cancelled. Return to Cinder Launcher.",
                    Some(Err(error)),
                ),
                Err(_) => (
                    "400 Bad Request",
                    "Sign-in response rejected. Return to Cinder Launcher.",
                    None,
                ),
            }
        };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{message}",
        message.len()
    );
    stream.write_all(response.as_bytes())?;
    Ok(result)
}

pub fn authenticate(client_id: &str) -> io::Result<microsoft_auth::OwnedSession> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let request = microsoft_auth::begin(client_id, port)?;
    webbrowser::open(&request.url)
        .map_err(|_| io::Error::other("Unable to open system browser"))?;
    let limit = Instant::now() + SIGN_IN_DEADLINE;
    loop {
        if Instant::now() >= limit {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Microsoft sign-in timed out",
            ));
        }
        match listener.accept() {
            Ok((mut stream, address)) if address.ip().is_loopback() => {
                if let Ok(Some(result)) = callback(&mut stream, &request, port) {
                    let code = result?;
                    return microsoft_auth::exchange_code(
                        &microsoft_auth::AuthHttp::new()?,
                        client_id,
                        &request,
                        &code,
                    );
                }
            }
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
            Err(err) => return Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_rejects_invalid_state_and_accepts_valid_code() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let request = microsoft_auth::begin("5fe9b6ce-72a4-468d-93ce-991b46667e26", port).unwrap();
        let url = reqwest::Url::parse(&request.url).unwrap();
        let state = url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .unwrap()
            .1
            .to_string();
        fn send(
            port: u16,
            path: String,
            request: &AuthRequest,
            listener: &TcpListener,
        ) -> (Option<io::Result<String>>, String) {
            let handle = std::thread::spawn(move || {
                let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
                stream
                    .write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
                    .unwrap();
                let mut buf = String::new();
                use std::io::Read;
                stream.read_to_string(&mut buf).unwrap();
                buf
            });
            let (mut stream, _) = listener.accept().unwrap();
            let result = callback(&mut stream, request, port).unwrap();
            drop(stream);
            (result, handle.join().unwrap())
        }
        let (bad, response) = send(
            port,
            "/callback?code=test&state=bad".into(),
            &request,
            &listener,
        );
        assert!(bad.is_none());
        assert!(response.contains("400 Bad Request"));
        let (good, response) = send(
            port,
            format!("/callback?code=example&state={state}"),
            &request,
            &listener,
        );
        assert_eq!(good.unwrap().unwrap(), "example");
        assert!(response.contains("Cache-Control: no-store"));
    }
}
