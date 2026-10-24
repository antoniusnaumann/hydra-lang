//! `hydra language-server`, driven message by message.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use hydra::lsp::Server;
use serde_json::{json, Value};

struct Session {
    server: Server,
    next: u64,
}

impl Session {
    fn new() -> Session {
        let mut session = Session { server: Server::new(), next: 1 };
        let reply = session.request("initialize", json!({ "capabilities": { "general": { "positionEncodings": ["utf-32", "utf-16"] } } }));
        assert_eq!(reply["capabilities"]["positionEncoding"], "utf-32");
        session
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        let out = self.server.handle(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let reply = out.into_iter().find(|m| m["id"] == id).expect("a response");
        assert!(reply.get("error").is_none(), "{reply}");
        reply["result"].clone()
    }

    /// Open a document and return the diagnostics published for it.
    fn open(&mut self, uri: &str, text: &str) -> Vec<Value> {
        let out = self.server.handle(json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": uri, "languageId": "hydra", "version": 1, "text": text }
        } }));
        out[0]["params"]["diagnostics"].as_array().cloned().unwrap_or_default()
    }

    fn change(&mut self, uri: &str, text: &str) -> Vec<Value> {
        let out = self.server.handle(json!({ "jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
            "textDocument": { "uri": uri, "version": 2 }, "contentChanges": [{ "text": text }]
        } }));
        out[0]["params"]["diagnostics"].as_array().cloned().unwrap_or_default()
    }

    fn at(&mut self, method: &str, uri: &str, line: u32, character: u32) -> Value {
        self.request(method, json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } }))
    }

    /// Open `text` with `|` marking the cursor, and ask `method` there.
    fn ask(&mut self, method: &str, uri: &str, text: &str) -> Value {
        let (line, col) = cursor(text);
        self.open(uri, &text.replace('|', ""));
        self.at(method, uri, line, col)
    }

    fn labels(&mut self, uri: &str, text: &str) -> Vec<String> {
        let items = self.ask("textDocument/completion", uri, text);
        items.as_array().expect("completion items").iter().map(|i| i["label"].as_str().unwrap().to_string()).collect()
    }
}

fn cursor(text: &str) -> (u32, u32) {
    for (line, content) in text.split('\n').enumerate() {
        if let Some(col) = content.chars().position(|c| c == '|') {
            return (line as u32, col as u32);
        }
    }
    panic!("no cursor in {text:?}");
}

fn dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hydra-lsp-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

fn uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

#[test]
fn completes_standard_module_functions_with_signatures_and_docs() {
    let mut s = Session::new();
    let items = s.ask("textDocument/completion", "file:///tmp/a.hy", "use fs\nfs::re|");
    let items = items.as_array().unwrap();
    let read = items.iter().find(|i| i["label"] == "read").expect("fs::read");
    assert_eq!(read["kind"], 3);
    assert_eq!(read["detail"], "fs::read(path) → string, reason (+1 overload)");
    let docs = read["documentation"]["value"].as_str().unwrap();
    assert!(docs.contains("fs::read(path, fallback)") && docs.contains("UTF-8"), "{docs}");
    // The edit replaces only the part of the name already typed.
    assert_eq!(read["textEdit"]["range"]["start"], json!({ "line": 1, "character": 4 }));
    assert_eq!(read["textEdit"]["newText"], "read");
    assert!(items.iter().all(|i| i["label"] != "print"), "only fs's names after fs::");
}

#[test]
fn follows_aliases_and_offers_every_module() {
    let mut s = Session::new();
    let labels = s.labels("file:///tmp/a.hy", "use list as l\nxs := l::|");
    assert!(labels.contains(&"map".to_string()) && labels.contains(&"sorted_by_key".to_string()), "{labels:?}");
    let labels = s.labels("file:///tmp/a.hy", "use |");
    for module in ["fs", "env", "text", "json", "io", "time", "http", "list", "cli"] {
        assert!(labels.contains(&module.to_string()), "{module} in {labels:?}");
    }
    let labels = s.labels("file:///tmp/a.hy", "x := ::|");
    assert!(labels.contains(&"print".to_string()) && labels.contains(&"push".to_string()));
}

#[test]
fn completes_names_in_scope_keywords_and_star_imports() {
    let mut s = Session::new();
    let text = "use text as *\n\n// Add two numbers.\nfn add(a, b = 1)\n\treturn a + b\nend\n\ncount := 0\npr|";
    let items = s.ask("textDocument/completion", "file:///tmp/a.hy", text);
    let items = items.as_array().unwrap();
    let find = |label: &str| items.iter().find(|i| i["label"] == label).unwrap_or_else(|| panic!("{label}"));
    assert_eq!(find("add")["detail"], "add(a, b = 1)");
    assert_eq!(find("add")["documentation"]["value"], "Add two numbers.");
    assert_eq!(find("count")["kind"], 6);
    assert_eq!(find("print")["kind"], 3);
    assert_eq!(find("split")["detail"], "split(text, separator = :null) → list");
    assert_eq!(find("parallel for")["kind"], 14);
    assert_eq!(find("text")["kind"], 9);
}

#[test]
fn completes_after_a_dot_and_atoms() {
    let mut s = Session::new();
    let labels = s.labels("file:///tmp/a.hy", "use list\nxs := [1, 2]\nxs.|");
    assert!(labels.contains(&"list".to_string()) && labels.contains(&"len".to_string()), "{labels:?}");
    let labels = s.labels("file:///tmp/a.hy", "use list\nxs := [1, 2]\nys := xs.list::so|");
    assert!(labels.contains(&"sorted".to_string()));
    let labels = s.labels("file:///tmp/a.hy", "mode := :fast\nx := :|");
    assert!(labels.contains(&"null".to_string()) && labels.contains(&"fast".to_string()) && labels.contains(&"not_found".to_string()));
}

#[test]
fn offers_nothing_inside_strings_and_comments_but_completes_interpolations() {
    let mut s = Session::new();
    assert_eq!(s.ask("textDocument/completion", "file:///tmp/a.hy", "print \"fs::|\""), json!([]));
    assert_eq!(s.ask("textDocument/completion", "file:///tmp/a.hy", "// fs::|"), json!([]));
    let labels = s.labels("file:///tmp/a.hy", "use text\nprint \"\\(text::up|\"");
    assert!(labels.contains(&"upper".to_string()));
}

#[test]
fn offers_named_arguments_inside_a_call() {
    let mut s = Session::new();
    let labels = s.labels("file:///tmp/a.hy", "use json\nx := json::stringify(v, pre|)");
    assert!(labels.contains(&"pretty =".to_string()), "{labels:?}");
}

#[test]
fn signature_help_tracks_overloads_and_the_active_argument() {
    let mut s = Session::new();
    let help = s.ask("textDocument/signatureHelp", "file:///tmp/a.hy", "use fs\nx := fs::read(\"a\", |");
    assert_eq!(help["activeSignature"], 1);
    assert_eq!(help["activeParameter"], 1);
    assert_eq!(help["signatures"][1]["label"], "fs::read(path, fallback)");
    // Offsets into the label: `fallback`.
    assert_eq!(help["signatures"][1]["parameters"][1]["label"], json!([15, 23]));

    // A call without parentheses, and a nested one.
    let help = s.ask("textDocument/signatureHelp", "file:///tmp/a.hy", "use text\nwords := text::split line, |");
    assert_eq!(help["signatures"][0]["label"], "text::split(text, separator = :null)");
    assert_eq!(help["activeParameter"], 1);
    let help = s.ask("textDocument/signatureHelp", "file:///tmp/a.hy", "print len([1, 2], |");
    assert_eq!(help["signatures"][0]["label"], "len(value)");

    // `x.f(…)`: the receiver is the first argument.
    let help = s.ask("textDocument/signatureHelp", "file:///tmp/a.hy", "use list\nxs.list::take(|");
    assert_eq!(help["activeParameter"], 1);

    // A named argument highlights the parameter it names.
    let help = s.ask("textDocument/signatureHelp", "file:///tmp/a.hy", "use io\nio::write \"x\", stream = |");
    assert_eq!(help["signatures"][0]["activeParameter"], 2);

    assert_eq!(s.ask("textDocument/signatureHelp", "file:///tmp/a.hy", "x := 1 - |"), Value::Null);
}

#[test]
fn hover_describes_functions_atoms_and_modules() {
    let mut s = Session::new();
    let hover = s.ask("textDocument/hover", "file:///tmp/a.hy", "use fs\nfs::ex|ists \"a\"");
    let text = hover["contents"]["value"].as_str().unwrap();
    assert!(text.starts_with("```hydra\nfs::exists(path)\n```"), "{text}");
    let hover = s.ask("textDocument/hover", "file:///tmp/a.hy", "x := :not_fo|und");
    assert!(hover["contents"]["value"].as_str().unwrap().contains("does not exist"));
    let hover = s.ask("textDocument/hover", "file:///tmp/a.hy", "use json\nx := js|on::parse \"1\"");
    assert!(hover["contents"]["value"].as_str().unwrap().contains("JSON"));
    let hover = s.ask("textDocument/hover", "file:///tmp/a.hy", "// Twice.\ndouble := fn(x) x * 2\nprint dou|ble(2)");
    assert_eq!(hover["contents"]["value"], "```hydra\ndouble(x)\n```\n\nTwice.");
}

#[test]
fn file_modules_complete_and_resolve_definitions() {
    let dir = dir("modules");
    std::fs::write(dir.join("helpers.hy"), "// Greet someone.\nfn greet(name)\n\tprint \"hi \\(name)\"\nend\n\nfn _hidden()\nend\n\nLIMIT := 3\n").unwrap();
    let main = dir.join("main.hy");
    let mut s = Session::new();
    let text = "use helpers\nhelpers::|";
    let items = s.ask("textDocument/completion", &uri(&main), text);
    let labels: Vec<&str> = items.as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap()).collect();
    assert_eq!(labels, ["greet", "LIMIT"]);
    assert_eq!(items[0]["detail"], "helpers::greet(name)");

    let labels = s.labels(&uri(&main), "use |");
    assert!(labels.contains(&"helpers".to_string()) && labels.contains(&"fs".to_string()));

    let location = s.ask("textDocument/definition", &uri(&main), "use helpers\nhelpers::gr|eet \"x\"");
    assert_eq!(location["uri"], uri(&dir.join("helpers.hy")));
    assert_eq!(location["range"]["start"], json!({ "line": 1, "character": 0 }));

    let location = s.ask("textDocument/definition", &uri(&main), "fn f()\nend\n\nf|()");
    assert_eq!(location["uri"], uri(&main));
    assert_eq!(location["range"]["start"]["line"], 0);
}

#[test]
fn publishes_syntax_errors_and_check_findings() {
    let mut s = Session::new();
    let diagnostics = s.open("file:///tmp/a.hy", "x := (1\n");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["code"], "syntax");
    let diagnostics = s.change("file:///tmp/a.hy", "print missing_name\n");
    assert!(diagnostics.iter().any(|d| d["source"] == "hydra check" && d["message"].as_str().unwrap().contains("missing_name")), "{diagnostics:?}");
    let first = &diagnostics[0]["range"];
    assert_eq!(first["start"], json!({ "line": 0, "character": 6 }));
    assert_eq!(first["end"], json!({ "line": 0, "character": 18 }));
    assert!(s.change("file:///tmp/a.hy", "print 1\n").is_empty());
}

#[test]
fn declarations_survive_an_edit_that_does_not_parse() {
    let mut s = Session::new();
    s.open("file:///tmp/a.hy", "fn helper(x)\n\treturn x\nend\n");
    s.change("file:///tmp/a.hy", "fn helper(x)\n\treturn x\nend\n\nprint hel(\n");
    let items = s.at("textDocument/completion", "file:///tmp/a.hy", 4, 9);
    let helper = items.as_array().unwrap().iter().find(|i| i["label"] == "helper").expect("helper");
    assert_eq!(helper["detail"], "helper(x)");
}

#[test]
fn formats_and_lists_symbols() {
    let mut s = Session::new();
    s.open("file:///tmp/a.hy", "fn  f( a ,b )\nreturn a\nend\nX := 1\n");
    let edits = s.request("textDocument/formatting", json!({ "textDocument": { "uri": "file:///tmp/a.hy" }, "options": { "tabSize": 4, "insertSpaces": false } }));
    assert_eq!(edits[0]["newText"], "fn f(a, b)\n\treturn a\nend\n\nX := 1\n");
    let symbols = s.request("textDocument/documentSymbol", json!({ "textDocument": { "uri": "file:///tmp/a.hy" } }));
    assert_eq!(symbols[0]["name"], "f");
    assert_eq!(symbols[0]["kind"], 12);
    assert_eq!(symbols[1]["name"], "X");
    assert_eq!(symbols[1]["kind"], 14);
}

#[test]
fn counts_columns_in_utf16_when_the_client_asks() {
    let mut server = Server::new();
    let out = server.handle(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "capabilities": {} } }));
    assert_eq!(out[0]["result"]["capabilities"]["positionEncoding"], "utf-16");
    server.handle(json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
        "textDocument": { "uri": "file:///tmp/a.hy", "languageId": "hydra", "version": 1, "text": "x := \"😀\"; use fs\nfs::rea" }
    } }));
    let out = server.handle(json!({ "jsonrpc": "2.0", "id": 2, "method": "textDocument/completion", "params": {
        "textDocument": { "uri": "file:///tmp/a.hy" }, "position": { "line": 1, "character": 7 }
    } }));
    let items = out[0]["result"].as_array().unwrap();
    assert!(items.iter().any(|i| i["label"] == "read"));
}

#[test]
fn speaks_the_protocol_over_stdio() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_hydra"))
        .arg("language-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    for message in [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "capabilities": {} } }),
        json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "shutdown" }),
        json!({ "jsonrpc": "2.0", "method": "exit" }),
    ] {
        let body = message.to_string();
        write!(input, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    }
    drop(input);
    let mut output = String::new();
    child.stdout.take().unwrap().read_to_string(&mut output).unwrap();
    assert!(child.wait().unwrap().success());
    assert!(output.starts_with("Content-Length: "), "{output}");
    assert!(output.contains("\"completionProvider\"") && output.contains("\"id\":2,\"result\":null"), "{output}");
}
