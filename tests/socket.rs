//! TCP socket end-to-end tests. Each drives the whole pipeline (parse → compile
//! → run on fusevm) over real `std::net` sockets on `127.0.0.1`. These pin the
//! MRI-faithful `TCPServer`/`TCPSocket` surface used to serve HTTP: `bind`+
//! `listen` via `TCPServer.new`, ephemeral-port readback via `#addr`, `#accept`,
//! and the socket read/write methods (`gets`/`read`/`write`).
//!
//! `eval_to_string` resets its own thread-local host, so the server and client
//! run on independent threads with independent interpreters — a genuine
//! two-party round-trip, not a self-send inside one host.

use rubylang::eval_to_string as ev;
use std::io::Write;

/// Poll a file until it holds a parseable `u16` port (written by the server
/// thread after it binds, before it blocks on `accept`). Bounded so a wedged
/// server fails the test instead of hanging CI forever.
fn wait_for_port(path: &std::path::Path) -> u16 {
    for _ in 0..600 {
        if let Ok(s) = std::fs::read_to_string(path) {
            if let Ok(p) = s.trim().parse::<u16>() {
                if p != 0 {
                    return p;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("server never published its port to {path:?}");
}

/// A rubylang `TCPServer` accepts one connection, reads the HTTP request line,
/// writes an HTTP/1.1 200 response, and a rubylang `TCPSocket` client — on a
/// separate thread with its own interpreter — reads the response back. The
/// server binds port 0 (OS-assigned) and publishes the real port via `#addr`.
#[test]
fn tcp_http_round_trip_between_two_interpreters() {
    let dir = tempfile::tempdir().expect("tempdir");
    let port_file = dir.path().join("port");
    let pf = port_file.to_str().unwrap().to_string();

    // Server thread: bind port 0, publish the bound port, accept, respond.
    let server_src = format!(
        r#"
        require 'socket'
        server = TCPServer.new('127.0.0.1', 0)
        File.write({pf:?}, server.addr[1].to_s)
        conn = server.accept
        request_line = conn.gets
        body = "Hello"
        conn.write("HTTP/1.1 200 OK\r\n")
        conn.write("Content-Length: #{{body.bytesize}}\r\n")
        conn.write("\r\n")
        conn.write(body)
        conn.close
        server.close
        request_line
        "#
    );
    let server = std::thread::spawn(move || ev(&server_src));

    let port = wait_for_port(&port_file);

    // Client thread: connect, send an HTTP request, read the whole response.
    let client_src = format!(
        r#"
        require 'socket'
        sock = TCPSocket.new('127.0.0.1', {port})
        sock.write("GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        resp = sock.read(nil)
        sock.close
        resp
        "#
    );
    let client = std::thread::spawn(move || ev(&client_src));

    let server_result = server.join().expect("server thread panicked");
    let client_result = client.join().expect("client thread panicked");

    // The server saw the client's request line (inspect form keeps the CRLF).
    let req = server_result.expect("server eval error");
    assert_eq!(req, "\"GET / HTTP/1.1\\r\\n\"", "server request line");

    // The client read the full HTTP response the server wrote.
    let resp = client_result.expect("client eval error");
    assert_eq!(
        resp, "\"HTTP/1.1 200 OK\\r\\nContent-Length: 5\\r\\n\\r\\nHello\"",
        "client response",
    );
}

/// The same round-trip, but the client is raw `std::net` (a non-rubylang peer),
/// proving the rubylang server speaks real TCP to anything on the wire — the
/// exact shape of serving a browser or `curl`.
#[test]
fn tcp_server_serves_a_raw_std_net_client() {
    let dir = tempfile::tempdir().expect("tempdir");
    let port_file = dir.path().join("port");
    let pf = port_file.to_str().unwrap().to_string();

    let server_src = format!(
        r#"
        require 'socket'
        server = TCPServer.new('127.0.0.1', 0)
        File.write({pf:?}, server.addr[1].to_s)
        conn = server.accept
        # Drain request line + headers up to the blank line.
        conn.gets
        while (line = conn.gets) && line != "\r\n"
        end
        body = "pong"
        conn.write("HTTP/1.1 200 OK\r\nContent-Length: #{{body.bytesize}}\r\nConnection: close\r\n\r\n")
        conn.write(body)
        conn.close
        server.close
        :served
        "#
    );
    let server = std::thread::spawn(move || ev(&server_src));

    let port = wait_for_port(&port_file);

    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .expect("write request");
    let mut resp = String::new();
    use std::io::Read;
    stream.read_to_string(&mut resp).expect("read response");

    let served = server
        .join()
        .expect("server thread panicked")
        .expect("server eval error");
    assert_eq!(served, ":served");

    assert!(
        resp.starts_with("HTTP/1.1 200 OK\r\n"),
        "response status line, got: {resp:?}"
    );
    assert!(
        resp.ends_with("\r\n\r\npong"),
        "response body, got: {resp:?}"
    );
}

/// `TCPServer#addr` reports the bound family/port/host, and a fresh `TCPServer`
/// is not `closed?` until `#close`.
#[test]
fn tcp_server_addr_and_close() {
    let src = r#"
        require 'socket'
        server = TCPServer.new('127.0.0.1', 0)
        fam = server.addr[0]
        host = server.addr[2]
        was_open = server.closed?
        server.close
        [fam, host, was_open, server.closed?]
    "#;
    match ev(src) {
        Ok(got) => assert_eq!(got, "[\"AF_INET\", \"127.0.0.1\", false, true]"),
        Err(e) => panic!("eval error: {e}"),
    }
}
