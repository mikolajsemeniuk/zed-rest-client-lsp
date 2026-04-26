mod variables;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

const SEND_COMMAND: &str = "http-lsp.send";

#[derive(Debug)]
struct Backend {
    client: Client,
    docs: Mutex<HashMap<Url, String>>,
    http: reqwest::Client,
}

impl Backend {
    fn new(client: Client) -> Self {
        Self {
            client,
            docs: Mutex::new(HashMap::new()),
            http: reqwest::Client::new(),
        }
    }
}

fn is_http_method_line(line: &str) -> bool {
    method_from_line(line).is_some()
}

fn method_from_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim_start();
    const METHODS: &[&str] = &[
        "GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS", "TRACE", "CONNECT",
    ];
    for m in METHODS {
        if let Some(rest) = trimmed.strip_prefix(m) {
            if let Some(url_part) = rest.strip_prefix(' ') {
                let url = extract_url(url_part.trim());
                if !url.is_empty() {
                    return Some(((*m).to_string(), url));
                }
            }
        }
    }
    None
}

fn extract_url(input: &str) -> String {
    let mut result = String::new();
    let mut chars = input.chars().peekable();
    let mut brace_depth: i32 = 0;

    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                result.push_str("{{");
                brace_depth += 1;
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                result.push_str("}}");
                brace_depth -= 1;
            }
            c if c.is_whitespace() && brace_depth <= 0 => {
                break;
            }
            c => result.push(c),
        }
    }

    result
}

#[derive(Debug)]
struct ParsedRequest {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<String>,
}

fn parse_request_at(text: &str, start_line: usize) -> Option<ParsedRequest> {
    let lines: Vec<&str> = text.lines().collect();
    if start_line >= lines.len() {
        return None;
    }
    let (method, url) = method_from_line(lines[start_line])?;

    let mut headers = Vec::new();
    let mut cursor = start_line + 1;

    while cursor < lines.len() {
        let line = lines[cursor];
        if line.trim().is_empty() {
            cursor += 1;
            break;
        }
        if line.starts_with("###") {
            return Some(ParsedRequest {
                method,
                url,
                headers,
                body: None,
            });
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
            cursor += 1;
        } else {
            break;
        }
    }

    let mut body_lines = Vec::new();
    while cursor < lines.len() {
        let line = lines[cursor];
        if line.starts_with("###") {
            break;
        }
        body_lines.push(line);
        cursor += 1;
    }
    let body = if body_lines.is_empty() {
        None
    } else {
        Some(body_lines.join("\n").trim().to_string()).filter(|s| !s.is_empty())
    };

    Some(ParsedRequest {
        method,
        url,
        headers,
        body,
    })
}

fn format_response(
    status: u16,
    status_text: &str,
    elapsed_ms: u128,
    content_type: Option<&str>,
    body: &str,
) -> String {
    let pretty_body = if let Some(ct) = content_type {
        if ct.contains("application/json") {
            serde_json::from_str::<serde_json::Value>(body)
                .ok()
                .and_then(|v| serde_json::to_string_pretty(&v).ok())
                .unwrap_or_else(|| body.to_string())
        } else {
            body.to_string()
        }
    } else {
        body.to_string()
    };

    format!(
        "HTTP/1.1 {} {} | {}ms\n\n{}",
        status, status_text, elapsed_ms, pretty_body
    )
}

fn write_response_to_tmp(
    content: &str,
    _content_type: Option<&str>,
) -> std::io::Result<(Url, PathBuf)> {
    let mut path = PathBuf::from(std::env::temp_dir());
    path.push("zed-http-response.json");

    std::fs::write(&path, content)?;

    let uri = Url::from_file_path(&path)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "invalid path"))?;
    Ok((uri, path))
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                code_lens_provider: Some(CodeLensOptions {
                    resolve_provider: Some(false),
                }),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec![SEND_COMMAND.to_string()],
                    work_done_progress_options: Default::default(),
                }),
                ..ServerCapabilities::default()
            },
            server_info: Some(ServerInfo {
                name: "http-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "http-lsp initialized")
            .await;
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let mut docs = self.docs.lock().unwrap();
        docs.insert(params.text_document.uri, params.text_document.text);
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.into_iter().next() {
            let mut docs = self.docs.lock().unwrap();
            docs.insert(params.text_document.uri, change.text);
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let mut docs = self.docs.lock().unwrap();
        docs.remove(&params.text_document.uri);
    }

    async fn code_lens(&self, params: CodeLensParams) -> Result<Option<Vec<CodeLens>>> {
        let uri = params.text_document.uri;
        let docs = self.docs.lock().unwrap();
        let Some(text) = docs.get(&uri) else {
            return Ok(Some(vec![]));
        };

        let mut lenses = Vec::new();
        for (idx, line) in text.lines().enumerate() {
            if is_http_method_line(line) {
                let range = Range {
                    start: Position {
                        line: idx as u32,
                        character: 0,
                    },
                    end: Position {
                        line: idx as u32,
                        character: line.len() as u32,
                    },
                };
                lenses.push(CodeLens {
                    range,
                    command: Some(Command {
                        title: "▶ Send Request".to_string(),
                        command: SEND_COMMAND.to_string(),
                        arguments: Some(vec![
                            serde_json::json!(uri.to_string()),
                            serde_json::json!(idx),
                        ]),
                    }),
                    data: None,
                });
            }
        }
        Ok(Some(lenses))
    }

    async fn execute_command(
        &self,
        params: ExecuteCommandParams,
    ) -> Result<Option<serde_json::Value>> {
        if params.command != SEND_COMMAND {
            return Ok(None);
        }

        let Some(uri_arg) = params.arguments.first() else {
            self.client
                .show_message(MessageType::ERROR, "Brak uri w argumentach")
                .await;
            return Ok(None);
        };
        let Some(line_arg) = params.arguments.get(1) else {
            self.client
                .show_message(MessageType::ERROR, "Brak line w argumentach")
                .await;
            return Ok(None);
        };

        let uri_str = uri_arg.as_str().unwrap_or("").to_string();
        let line_idx = line_arg.as_u64().unwrap_or(0) as usize;

        let Ok(uri) = Url::parse(&uri_str) else {
            self.client
                .show_message(MessageType::ERROR, "Nieprawidłowe URI")
                .await;
            return Ok(None);
        };

        let text = {
            let docs = self.docs.lock().unwrap();
            docs.get(&uri).cloned()
        };
        let Some(text) = text else {
            self.client
                .show_message(MessageType::ERROR, "Dokument nie jest otwarty")
                .await;
            return Ok(None);
        };

        let Some(mut req) = parse_request_at(&text, line_idx) else {
            self.client
                .show_message(MessageType::ERROR, "Nie udało się sparsować requestu")
                .await;
            return Ok(None);
        };

        let file_vars = variables::parse_file_variables(&text);
        match variables::substitute_all(&req.url, &file_vars) {
            Ok(v) => req.url = v,
            Err(e) => {
                self.client
                    .show_message(MessageType::ERROR, format!("URL: {e}"))
                    .await;
                return Ok(None);
            }
        }
        for (k, v) in req.headers.iter_mut() {
            if let Ok(new_k) = variables::substitute_all(k, &file_vars) {
                *k = new_k;
            }
            match variables::substitute_all(v, &file_vars) {
                Ok(new_v) => *v = new_v,
                Err(e) => {
                    self.client
                        .show_message(MessageType::ERROR, format!("header: {e}"))
                        .await;
                    return Ok(None);
                }
            }
        }
        if let Some(body) = &req.body {
            match variables::substitute_all(body, &file_vars) {
                Ok(new_body) => req.body = Some(new_body),
                Err(e) => {
                    self.client
                        .show_message(MessageType::ERROR, format!("body: {e}"))
                        .await;
                    return Ok(None);
                }
            }
        }

        let method = match req.method.as_str() {
            "GET" => reqwest::Method::GET,
            "POST" => reqwest::Method::POST,
            "PUT" => reqwest::Method::PUT,
            "DELETE" => reqwest::Method::DELETE,
            "PATCH" => reqwest::Method::PATCH,
            "HEAD" => reqwest::Method::HEAD,
            "OPTIONS" => reqwest::Method::OPTIONS,
            other => {
                self.client
                    .show_message(
                        MessageType::ERROR,
                        format!("Nieobsługiwana metoda: {other}"),
                    )
                    .await;
                return Ok(None);
            }
        };

        let mut builder = self.http.request(method, &req.url);
        for (k, v) in &req.headers {
            builder = builder.header(k, v);
        }
        if let Some(body) = &req.body {
            builder = builder.body(body.clone());
        }

        let start = Instant::now();
        let result = builder.send().await;
        let elapsed = start.elapsed().as_millis();

        let (formatted, content_type) = match result {
            Ok(resp) => {
                let status = resp.status();
                let status_u16 = status.as_u16();
                let status_text = status.canonical_reason().unwrap_or("").to_string();
                let content_type = resp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());
                let body = resp
                    .text()
                    .await
                    .unwrap_or_else(|e| format!("<read error: {e}>"));
                let formatted = format_response(
                    status_u16,
                    &status_text,
                    elapsed,
                    content_type.as_deref(),
                    &body,
                );
                (formatted, content_type)
            }
            Err(e) => (format!("REQUEST ERROR | {elapsed}ms\n\n{e}"), None),
        };

        let (_response_uri, response_path) =
            match write_response_to_tmp(&formatted, content_type.as_deref()) {
                Ok(t) => t,
                Err(e) => {
                    self.client
                        .show_message(MessageType::ERROR, format!("zapis do tmp poległ: {e}"))
                        .await;
                    return Ok(Some(serde_json::json!({ "ok": true, "saved_file": false })));
                }
            };

        if let Err(e) = std::process::Command::new("zed")
            .arg(&response_path)
            .spawn()
        {
            self.client
                .show_message(
                    MessageType::ERROR,
                    format!("nie udało się odpalić `zed` CLI: {e}. Sprawdź czy CLI jest zainstalowane (cmd+shift+p → cli: install)"),
                )
                .await;
        }

        Ok(Some(serde_json::json!({ "ok": true })))
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
