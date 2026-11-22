use super::*;
use std::{
    io::{BufRead, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
};

fn server(responses: Vec<String>) -> (String, std::thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/artifact?access_token=must-not-leak",
        listener.local_addr().unwrap()
    );
    let worker = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for response in responses {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = std::io::BufReader::new(&socket);
            let mut request = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                request.push_str(&line);
            }
            requests.push(request);
            drop(reader);
            socket.write_all(response.as_bytes()).unwrap();
        }
        requests
    });
    (url, worker)
}

fn response(status: u16, headers: &str, body: &str) -> String {
    format!("HTTP/1.1 {status} Test\r\nConnection: close\r\n{headers}\r\n{body}")
}

#[test]
fn transient_statuses_retry_with_retry_after_and_return_the_actual_body() {
    let (url, server) = server(vec![
        response(503, "Content-Length: 0\r\nRetry-After: 2\r\n", ""),
        response(504, "Content-Length: 0\r\n", ""),
        response(200, "Content-Length: 4\r\n", "done"),
    ]);
    let waits = Mutex::new(Vec::new());
    let body = bytes_with(&url, || request(&url), &|wait| {
        waits.lock().unwrap().push(wait)
    })
    .unwrap();
    assert_eq!(body, b"done");
    assert_eq!(server.join().unwrap().len(), 3);
    let waits = waits.into_inner().unwrap();
    assert_eq!(waits.len(), 2);
    assert!(waits[0] >= Duration::from_secs(2));
    assert!((Duration::from_millis(500)..=Duration::from_millis(750)).contains(&waits[1]));
}

#[test]
fn persistent_outages_stop_after_four_attempts_and_redact_sensitive_urls() {
    let (url, server) = server(vec![response(503, "Content-Length: 0\r\n", ""); 4]);
    let error = bytes_with(&url, || request(&url), &|_| {}).unwrap_err();
    assert!(error.contains("HTTP 503"));
    assert!(!error.contains("access_token") && !error.contains("must-not-leak"));
    assert_eq!(server.join().unwrap().len(), 4);
}

#[test]
fn permanent_errors_and_long_retry_after_do_not_retry_early() {
    for (status, headers) in [
        (404, "Content-Length: 0\r\n"),
        (503, "Content-Length: 0\r\nRetry-After: 120\r\n"),
    ] {
        let (url, server) = server(vec![response(status, headers, "")]);
        assert!(bytes_with(&url, || request(&url), &|_| panic!(
            "unexpected retry"
        ))
        .is_err());
        assert_eq!(server.join().unwrap().len(), 1);
    }
}

#[test]
fn interrupted_body_is_restarted_and_never_returned_as_complete() {
    let (url, server) = server(vec![
        response(200, "Content-Length: 8\r\n", "bro"),
        response(200, "Content-Length: 8\r\n", "complete"),
    ]);
    assert_eq!(
        bytes_with(&url, || request(&url), &|_| {}).unwrap(),
        b"complete"
    );
    assert_eq!(server.join().unwrap().len(), 2);
}

#[test]
fn read_timeouts_are_bounded_and_reported_without_response_or_url_secrets() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/?code=private-code",
        listener.local_addr().unwrap()
    );
    let sockets = Arc::new(Mutex::new(Vec::new()));
    let held = sockets.clone();
    let worker = std::thread::spawn(move || {
        for _ in 0..4 {
            held.lock().unwrap().push(listener.accept().unwrap().0);
        }
    });
    let call = || {
        classify(
            &url,
            ureq::AgentBuilder::new()
                .timeout_read(Duration::from_millis(25))
                .build()
                .get(&url)
                .call(),
        )
    };
    let error = bytes_with(&url, call, &|_| {}).unwrap_err();
    assert!(error.contains("timed out"), "{error}");
    assert!(!error.contains("private-code"));
    worker.join().unwrap();
    assert_eq!(sockets.lock().unwrap().len(), 4);
}

#[test]
fn http_date_retry_after_does_not_get_retried_before_the_server_allows() {
    let date = (chrono::Utc::now() + chrono::Duration::seconds(120)).to_rfc2822();
    let (url, worker) = server(vec![response(
        503,
        &format!("Content-Length: 0\r\nRetry-After: {date}\r\n"),
        "",
    )]);
    assert!(bytes_with(&url, || request(&url), &|_| panic!(
        "retried too early"
    ))
    .is_err());
    assert_eq!(worker.join().unwrap().len(), 1);
}
