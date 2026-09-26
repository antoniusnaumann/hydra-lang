use hydra::value::to_text;
use hydra::vm::{run_source, Options, RunResult};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn run(src: &str) -> RunResult {
    run_source(
        src,
        "modules-test.hy",
        Options {
            threads: 1,
            step_budget: 1,
            search_path: vec![],
            script_args: vec!["hello world".into(), "--quiet".into()],
            ..Options::default()
        },
    )
    .expect("parse")
}
fn value(result: &RunResult, name: &str) -> String {
    assert!(result.ok(), "{:?}", result.crash);
    to_text(
        &result
            .root_scope
            .lookup(name)
            .unwrap_or_else(|| panic!("missing {name}"))
            .read()
            .unwrap(),
    )
}
fn q(text: &str) -> String {
    serde_json::to_string(text).unwrap()
}

#[test]
fn all_modules_import_and_alias_and_star() {
    let r = run("use text as t\nuse json as *\nuse env\nuse time\nuse io\nuse http\nx := t::upper(\"hello\")\ny := parse(\"true\")\na := env::args()\np := env::platform()\n");
    assert_eq!(value(&r, "x"), "HELLO");
    assert_eq!(value(&r, "y"), ":true");
    assert_eq!(value(&r, "a"), "[hello world, --quiet]");
    assert_eq!(value(&r, "p"), format!(":{}", std::env::consts::OS));
}

#[test]
fn environment_missing_and_invalid_names() {
    let r = run("use env\nv, reason := env::get(\"HYDRA_TEST_UNSET_349718750\", \"fallback\")\nall := env::all()\n");
    assert_eq!(value(&r, "v"), "fallback");
    assert_eq!(value(&r, "reason"), ":not_found");
    assert!(
        run("use env\nx := env::get(\"HYDRA_TEST_UNSET_349718750\")")
            .crash
            .is_some()
    );
    assert!(run("use env\nx := env::get(\"=invalid\", :null)")
        .crash
        .is_some());
}

#[test]
fn text_is_unicode_aware_and_preserves_empty_fields() {
    let r = run(r#"use text
parts := text::split(" a\tβ \n😀 ")
fields := text::split("a,,b,", ",")
chars := text::split("é😀", "")
joined := text::join(chars, "-")
trimmed := text::trim("  é \n")
index := text::find("é😀z", "z")
missing := text::find("abc", "z")
replaced := text::replace("aaa", "a", "β")
lower := text::lower("ÄBC")
upper := text::upper("straße")
prefix := text::starts_with("éx", "é")
suffix := text::ends_with("éx", "x")
"#);
    for (name, expected) in [
        ("parts", "[a, β, 😀]"),
        ("fields", "[a, , b, ]"),
        ("chars", "[é, 😀]"),
        ("joined", "é-😀"),
        ("trimmed", "é"),
        ("index", "2"),
        ("missing", ":null"),
        ("replaced", "βββ"),
        ("lower", "äbc"),
        ("upper", "STRASSE"),
        ("prefix", ":true"),
        ("suffix", ":true"),
    ] {
        assert_eq!(value(&r, name), expected, "{name}");
    }
    assert!(run("use text\nx := text::join([1])").crash.is_some());
}

#[test]
fn json_roundtrip_atoms_and_quoted_keys() {
    let r = run(r#"use json
original := { :name : "Hydra", :active : :true, :missing : :null, :list : [1, 2], :"odd key" : "é😀" }
encoded := json::stringify(original)
decoded, reason := json::parse(encoded)
same := original == decoded
atom := json::stringify(:ready)
pretty := json::stringify(original, pretty = :true)
fallback, invalid := json::parse("{broken", 42)
"#);
    assert_eq!(value(&r, "same"), ":true");
    assert_eq!(value(&r, "reason"), ":null");
    assert_eq!(value(&r, "atom"), "\"ready\"");
    assert_eq!(value(&r, "fallback"), "42");
    assert_eq!(value(&r, "invalid"), ":invalid");
    assert!(value(&r, "pretty").contains('\n'));
    assert!(run("use json\nx := json::parse(\"1 garbage\")")
        .crash
        .is_some());
    assert!(run("use json\nx := json::stringify(print)").crash.is_some());
    assert!(run("use json\nx := json::stringify(1 / 0)").crash.is_some());
}

#[test]
fn dates_offsets_pre_epoch_and_bad_formats() {
    let r = run(r#"use time
a, reason := time::parse("1970-01-01T01:00:00+01:00")
b := time::parse("1969-12-31T23:59:59.500Z")
c := time::format(b, "%Y-%m-%d %H:%M:%S%.3f %Z")
d, bad := time::parse("tomorrow", :null)
now := time::now()
"#);
    assert_eq!(value(&r, "a"), "0");
    assert_eq!(value(&r, "reason"), ":null");
    assert_eq!(value(&r, "b"), "-0.5");
    assert_eq!(value(&r, "c"), "1969-12-31 23:59:59.500 UTC");
    assert_eq!(value(&r, "bad"), ":invalid");
    assert!(run("use time\nx := time::format(0, \"%Q\")")
        .crash
        .is_some());
    assert!(run("use time\nx := time::sleep(-1)").crash.is_some());
}

#[test]
fn sleep_yields_even_with_one_scheduler_worker() {
    let r = run("use time\nstate := { :ready : :false, :observed : :false }\nparallel\n time::sleep(0.1)           || state.ready = :true\n state.observed = state.ready ||\nend\nresult := state.observed\n");
    assert_eq!(value(&r, "result"), ":true");
}

#[test]
fn race_cancels_assignment_but_finishes_pending_call() {
    let r = run("use time\nfn wait()\n time::sleep(0.08)\n return 99\nend\n\nx := 0\nrace\n x = wait() || time::sleep(0.01)\nend\n");
    assert_eq!(value(&r, "x"), "0");
}

fn server(response: Vec<u8>, delay: Duration) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local HTTP fixture");
    listener.set_nonblocking(true).unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(v) => break v,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(e) => panic!("HTTP fixture accept: {e}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
        let header = String::from_utf8(request.clone()).unwrap();
        let length = header
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        let mut body = vec![0; length];
        stream.read_exact(&mut body).unwrap();
        request.extend(body);
        std::thread::sleep(delay);
        let _ = stream.write_all(&response);
        String::from_utf8(request).unwrap()
    });
    (address, handle)
}
fn response(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
    let mut result = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
        body.len()
    )
    .into_bytes();
    result.extend(body);
    result
}

#[test]
fn http_post_headers_and_non_success_status_are_data() {
    let (url, server) = server(
        response("404 Not Found", "X-Multi: a\r\nX-Multi: b\r\n", b"missing"),
        Duration::ZERO,
    );
    let r = run(&format!("use http\nr, reason := http::post({}, \"hello\", headers = {{ :x-test : \"works\" }})\nstatus := r.status\nbody := r.body\nheaders := r.headers[:x-multi]\n", q(&url)));
    assert_eq!(value(&r, "status"), "404");
    assert_eq!(value(&r, "body"), "missing");
    assert_eq!(value(&r, "headers"), "[a, b]");
    assert_eq!(value(&r, "reason"), ":null");
    let request = server.join().unwrap();
    assert!(request.starts_with("POST / HTTP/1.1"));
    assert!(request.to_ascii_lowercase().contains("x-test: works"));
    assert!(request.ends_with("hello"));
}

#[test]
fn http_limits_encoding_and_timeout_have_fallbacks() {
    for (body, options, delay, reason) in [
        (
            b"abc".to_vec(),
            "max_bytes = 2",
            Duration::ZERO,
            ":too_large",
        ),
        (vec![255], "timeout = 2", Duration::ZERO, ":encoding"),
        (
            b"late".to_vec(),
            "timeout = 0.03",
            Duration::from_millis(150),
            ":timeout",
        ),
    ] {
        let (url, server) = server(response("200 OK", "", &body), delay);
        let r = run(&format!(
            "use http\nr, reason := http::get({}, :null, {options})\n",
            q(&url)
        ));
        assert_eq!(value(&r, "reason"), reason);
        assert_eq!(value(&r, "r"), ":null");
        server.join().unwrap();
    }
}

#[test]
fn http_request_method_and_single_worker_progress() {
    let (url, server) = server(response("200 OK", "", b"ok"), Duration::from_millis(100));
    let r = run(&format!("use http\nstate := {{ :ready : :false, :seen : :false }}\nparallel\n r := http::request(:put, {}, body = \"data\") || state.ready = :true\n state.seen = state.ready ||\nend\nresult := state.seen\n", q(&url)));
    assert_eq!(value(&r, "result"), ":true");
    assert!(server.join().unwrap().starts_with("PUT /"));
}

#[test]
fn http_follows_redirects() {
    let (target, done) = server(response("200 OK", "", b"redirected"), Duration::ZERO);
    let (url, redirect) = server(
        response("302 Found", &format!("Location: {target}/new\r\n"), b""),
        Duration::ZERO,
    );
    let r = run(&format!(
        "use http\nr := http::get({})\nbody := r.body\nurl := r.url\n",
        q(&url)
    ));
    assert_eq!(value(&r, "body"), "redirected");
    assert_eq!(value(&r, "url"), format!("{target}/new"));
    redirect.join().unwrap();
    done.join().unwrap();
}

#[test]
fn downloads_keep_binary_bytes_and_preserve_target_on_failure() {
    let dir = std::env::temp_dir().join(format!("hydra-http-test-{}", std::process::id()));
    let path = dir.join("download.bin");
    let (url, server) = server(response("200 OK", "", &[0, 255, 1]), Duration::ZERO);
    let r = run(&format!(
        "use http\np, reason := http::download({}, {})",
        q(&url),
        q(path.to_str().unwrap())
    ));
    assert_eq!(value(&r, "reason"), ":null");
    assert_eq!(std::fs::read(&path).unwrap(), vec![0, 255, 1]);
    server.join().unwrap();
    let (url, server) = self::server(response("500 Error", "", b"oops"), Duration::ZERO);
    let r = run(&format!(
        "use http\np, reason := http::download({}, {}, :null)",
        q(&url),
        q(path.to_str().unwrap())
    ));
    assert_eq!(value(&r, "reason"), ":http_status");
    assert_eq!(std::fs::read(&path).unwrap(), vec![0, 255, 1]);
    server.join().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn io_and_cli_arguments_work_in_a_real_script() {
    let path = std::env::temp_dir().join(format!("hydra-io-test-{}.hy", std::process::id()));
    std::fs::write(&path, "use io\nuse env\na := io::read_line()\nb := io::read_line()\nc := io::read_line()\n_ = io::write(a)\n_ = io::write(b, stream = :stderr)\nprint c\nprint env::args()\nprint io::is_terminal(:stdin)\n").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_hydra"))
        .args([
            "run",
            path.to_str().unwrap(),
            "--",
            "hello world",
            "--dump-scope",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"first\r\nsecond\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    std::fs::remove_file(path).unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "first:null\n[hello world, --dump-scope]\n:false\n"
    );
    assert_eq!(String::from_utf8(out.stderr).unwrap(), "second");
}

#[test]
fn checker_knows_every_new_module_and_signature() {
    let source = "use env\nuse text\nuse json\nuse io\nuse time\nuse http\nargs := env::args()\ntext := text::join(args)\nx, reason := json::parse(text, :null)\ny := time::format(0)\nz := http::get(\"http://localhost\", :null, timeout = 1)\n";
    // Runtime import/arity coverage without performing the illustrative request.
    for module in ["env", "text", "json", "io", "time", "http"] {
        assert!(!hydra::value::Native::module_names(module).is_empty());
    }
    let program = hydra::parser::parse(source, "test.hy").unwrap();
    let report = hydra::check::check_program(
        &program,
        &hydra::check::CheckOptions {
            externs: vec![],
            search_path: vec![],
        },
    );
    assert!(
        !report
            .sorted()
            .iter()
            .any(|d| d.severity == hydra::errors::Severity::Error),
        "{:?}",
        report.sorted()
    );
}

#[test]
fn repeated_immediate_waits_deliver_results_without_lost_wakes() {
    for threads in [1, 4] {
        let r = run_source(
            "use time\ni := 0\nwhile i < 60\n time::sleep(0)\n i += 1\nend\n",
            "wake-test.hy",
            Options {
                threads,
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(value(&r, "i"), "60");
    }
}

#[test]
fn input_readers_report_encoding_and_collect_lines() {
    for (name, source, input, expected) in [
        (
            "read",
            "v, reason := io::read(:null)\nprint reason\n",
            vec![255],
            ":encoding\n",
        ),
        (
            "lines",
            "v := io::lines()\nprint v\n",
            b"a\r\nb\n".to_vec(),
            "[a, b]\n",
        ),
        (
            "raw",
            "v := io::read()\n_ = io::write(v)\n",
            b"a\r\nb\n".to_vec(),
            "a\r\nb\n",
        ),
    ] {
        let path = std::env::temp_dir().join(format!("hydra-io-{name}-{}.hy", std::process::id()));
        std::fs::write(&path, format!("use io\n{source}")).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_hydra"))
            .args(["run", path.to_str().unwrap()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&input).unwrap();
        let output = child.wait_with_output().unwrap();
        std::fs::remove_file(path).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
    }
}

#[test]
fn invalid_http_arguments_crash_even_with_a_fallback() {
    for call in [
        "http::get(\"file:///tmp/file\", :null)",
        "http::get(\"http://localhost\", :null, timeout = 0)",
        "http::get(\"http://localhost\", :null, max_bytes = -1)",
        "http::get(\"http://localhost\", :null, headers = [])",
        "http::download(\"http://localhost\", 42, :null)",
        "http::request(\"BAD METHOD\", \"http://localhost\", :null)",
    ] {
        let r = run(&format!("use http\nr := {call}"));
        let crash = r.crash.expect(call);
        assert!(
            crash.site.pos.is_known(),
            "asynchronous crash must retain its call site"
        );
    }
}

#[test]
fn timeout_in_response_body_is_reported() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(e) => panic!("accept: {e}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\n")
            .unwrap();
        std::thread::sleep(Duration::from_millis(150));
        let _ = stream.write_all(b"late");
    });
    let r = run(&format!(
        "use http\nr, reason := http::get({}, :null, timeout = 0.04)",
        q(&url)
    ));
    assert_eq!(value(&r, "reason"), ":timeout");
    server.join().unwrap();
}

#[test]
fn https_rejects_untrusted_certificates_as_tls_failure() {
    use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
    let cert =
        CertificateDer::from_pem_slice(include_bytes!("fixtures/tls/test-cert.pem")).unwrap();
    let key = PrivateKeyDer::from_pem_slice(include_bytes!("fixtures/tls/test-key.pem")).unwrap();
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("https://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(e) => panic!("TLS fixture accept: {e}"),
            }
        };
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut connection = rustls::ServerConnection::new(std::sync::Arc::new(config)).unwrap();
        let mut tls = rustls::Stream::new(&mut connection, &mut socket);
        let _ = tls.read(&mut [0; 1]); // Client should reject the handshake.
    });
    let r = run(&format!(
        "use http\nr, reason := http::get({}, :null, timeout = 2)",
        q(&url)
    ));
    assert_eq!(value(&r, "r"), ":null");
    assert_eq!(value(&r, "reason"), ":tls");
    server.join().unwrap();
}

#[test]
fn every_http_shorthand_sends_its_method_body_and_options() {
    for method in [
        "get", "head", "post", "put", "patch", "delete", "options", "connect", "trace",
    ] {
        for fallback in [false, true] {
            let no_response_body = matches!(method, "head" | "connect");
            // HEAD's content-length describes the resource, not a response body.
            let wire_response = if method == "head" {
                b"HTTP/1.1 200 OK\r\nContent-Length: 999\r\nConnection: close\r\n\r\n".to_vec()
            } else {
                response("200 OK", "", if no_response_body { b"" } else { b"ok" })
            };
            let (url, server) = server(wire_response, Duration::ZERO);
            let positional_body = matches!(method, "post" | "put" | "patch");
            let named_body = matches!(method, "delete" | "options");
            let mut args = q(&url);
            if positional_body {
                args.push_str(", \"payload\"");
            }
            if fallback {
                args.push_str(", :null");
            }
            if named_body {
                args.push_str(", body = \"payload\"");
            }
            args.push_str(", headers = { :x-test : \"shorthand\" }, timeout = 2, max_bytes = 32");
            let r = run(&format!("use http\nr, reason := http::{method}({args})\nstatus := r.status\nbody := r.body\n"));
            assert_eq!(value(&r, "status"), "200", "{method}");
            assert_eq!(value(&r, "reason"), ":null", "{method}");
            assert_eq!(
                value(&r, "body"),
                if no_response_body { "" } else { "ok" },
                "{method}"
            );
            let sent = server.join().unwrap();
            let target = if method == "connect" {
                url.strip_prefix("http://").unwrap()
            } else {
                "/"
            };
            assert!(
                sent.starts_with(&format!("{} {target} HTTP/1.1\r\n", method.to_uppercase())),
                "{sent}"
            );
            assert!(
                sent.to_lowercase().contains("x-test: shorthand\r\n"),
                "{sent}"
            );
            let body = sent.split_once("\r\n\r\n").unwrap().1;
            assert_eq!(
                body,
                if positional_body || named_body {
                    "payload"
                } else {
                    ""
                },
                "{method}"
            );
        }
    }
}

#[test]
fn every_new_http_shorthand_reports_transport_failures() {
    for method in [
        "head", "put", "patch", "delete", "options", "connect", "trace",
    ] {
        for fallback in [false, true] {
            let (url, server) = server(b"not an HTTP response\r\n\r\n".to_vec(), Duration::ZERO);
            let body = if matches!(method, "put" | "patch") {
                ", \"payload\""
            } else {
                ""
            };
            let fallback_arg = if fallback { ", 42" } else { "" };
            let r = run(&format!(
                "use http\nr, reason := http::{method}({}{body}{fallback_arg}, timeout = 2)",
                q(&url)
            ));
            if fallback {
                assert_eq!(value(&r, "r"), "42", "{method}");
                assert_eq!(value(&r, "reason"), ":network", "{method}");
            } else {
                assert!(
                    r.crash.is_some(),
                    "{method} should crash without a fallback"
                );
            }
            server.join().unwrap();
        }
    }
}
