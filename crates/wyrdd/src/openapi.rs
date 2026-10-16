//! The `OpenAPI` 3.1 document of the daemon API, built from the same
//! schemars schemas as the event schema.

use schemars::JsonSchema;
use schemars::generate::{SchemaGenerator, SchemaSettings};
use serde_json::{Map, Value, json};

use crate::api;
use crate::problem::Problem;

/// Who may call an operation over TCP (the unix socket admits the
/// daemon's own UID for every route).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    /// The daemon bearer token.
    Daemon,
    /// The per-session ingest token.
    Ingest,
}

/// A response body: JSON with a schema, or another media type.
#[derive(Debug, Clone)]
pub enum Body {
    Json(Value),
    Text(&'static str, &'static str),
}

/// One documented operation.
#[derive(Debug, Clone)]
pub struct Operation {
    pub method: &'static str,
    pub path: &'static str,
    pub summary: &'static str,
    pub auth: Auth,
    pub request: Option<Value>,
    /// Success status, description, body.
    pub ok: (u16, &'static str, Body),
    /// Extra success answers (e.g. 202 for relaunch).
    pub also: Vec<(u16, &'static str, Body)>,
    /// Problem statuses documented besides 401 and 500.
    pub problems: &'static [u16],
}

struct Schemas {
    out: SchemaGenerator,
    inp: SchemaGenerator,
}

impl Schemas {
    fn new() -> Self {
        let generator = |s: SchemaSettings| {
            s.with(|s| s.definitions_path = "#/components/schemas/".into())
                .into_generator()
        };
        Schemas {
            out: generator(SchemaSettings::draft2020_12().for_serialize()),
            inp: generator(SchemaSettings::draft2020_12().for_deserialize()),
        }
    }

    fn out<T: JsonSchema>(&mut self) -> Body {
        Body::Json(self.out.subschema_for::<T>().to_value())
    }

    fn inp<T: JsonSchema>(&mut self) -> Value {
        self.inp.subschema_for::<T>().to_value()
    }
}

const SESSION: &[u16] = &[400, 404, 409];
const ACTION: &[u16] = &[400, 404, 409, 422];
const INGEST: &[u16] = &[400, 404, 409, 413, 415, 422];

fn op(
    method: &'static str,
    path: &'static str,
    summary: &'static str,
    ok: (u16, &'static str, Body),
) -> Operation {
    Operation {
        method,
        path,
        summary,
        auth: Auth::Daemon,
        request: None,
        ok,
        also: Vec::new(),
        problems: SESSION,
    }
}

// A declarative table: one entry per operation reads better than helpers.
#[allow(clippy::too_many_lines)]
fn operations(s: &mut Schemas) -> Vec<Operation> {
    let relaunch = |s: &mut Schemas| {
        vec![(
            202,
            "Handed to a detached wyrd with --relaunch",
            s.out::<api::Relaunching>(),
        )]
    };
    let mut v = vec![
        Operation {
            problems: &[],
            ..op(
                "get",
                "/v1/health",
                "Liveness and version",
                (200, "Alive", s.out::<api::Health>()),
            )
        },
        Operation {
            problems: &[503],
            ..op(
                "get",
                "/v1/ready",
                "Data directory usable",
                (200, "Ready", s.out::<api::Ready>()),
            )
        },
        Operation {
            problems: &[],
            ..op(
                "get",
                "/v1/capabilities",
                "Kernel capability report (wyrd doctor)",
                (200, "Report", s.out::<wyrd_doctor::Report>()),
            )
        },
        Operation {
            problems: &[],
            ..op(
                "get",
                "/v1/metrics",
                "Prometheus metrics",
                (
                    200,
                    "Text exposition",
                    Body::Text("text/plain", "Prometheus text format 0.0.4"),
                ),
            )
        },
        Operation {
            problems: &[],
            ..op(
                "get",
                "/v1/sessions",
                "List sessions, newest first",
                (200, "Sessions", s.out::<api::SessionList>()),
            )
        },
        op(
            "get",
            "/v1/sessions/{sid}",
            "One session with its ledger head",
            (200, "Session", s.out::<api::SessionDetail>()),
        ),
        op(
            "get",
            "/v1/sessions/{sid}/events",
            "A page of ledger events",
            (200, "Events", s.out::<api::EventPage>()),
        ),
        op(
            "get",
            "/v1/sessions/{sid}/graph",
            "Causal graph (correlator)",
            (200, "Graph", s.out::<wyrd_correlator::Graph>()),
        ),
        op(
            "get",
            "/v1/sessions/{sid}/tool-calls/{tcid}",
            "One tool call and its effects",
            (200, "Call", s.out::<api::ToolCallDetail>()),
        ),
        op(
            "get",
            "/v1/sessions/{sid}/stream",
            "Live ledger events as Server-Sent Events, resumable with Last-Event-ID",
            (
                200,
                "SSE: event `ledger` (id = seq, data = Event JSON); final event `end` (data = last seq)",
                Body::Text("text/event-stream", "Server-Sent Events"),
            ),
        ),
        op(
            "get",
            "/v1/sessions/{sid}/verify",
            "Verify the hash chain and seal",
            (200, "Verification", s.out::<api::VerifyReport>()),
        ),
        op(
            "get",
            "/v1/sessions/{sid}/checkpoints",
            "Checkpoints and branches",
            (200, "Checkpoints", s.out::<api::Checkpoints>()),
        ),
        op(
            "post",
            "/v1/sessions/{sid}/checkpoints",
            "Checkpoint the current branch",
            (
                201,
                "Checkpoint",
                s.out::<wyrd_cell::state::CheckpointRecord>(),
            ),
        ),
        op(
            "post",
            "/v1/sessions/{sid}/stop",
            "Stop the running agent",
            (200, "Stopped", s.out::<api::Stopped>()),
        ),
        Operation {
            request: Some(s.inp::<api::RewindBody>()),
            also: relaunch(s),
            problems: ACTION,
            ..op(
                "post",
                "/v1/sessions/{sid}/checkpoints/{target}/rewind",
                "Rewind to a checkpoint (new branch)",
                (200, "New branch", s.out::<api::BranchCreated>()),
            )
        },
        Operation {
            request: Some(s.inp::<api::ForkBody>()),
            also: relaunch(s),
            problems: ACTION,
            ..op(
                "post",
                "/v1/sessions/{sid}/checkpoints/{target}/fork",
                "Fork a named branch",
                (201, "New branch", s.out::<api::BranchCreated>()),
            )
        },
        Operation {
            request: Some(s.inp::<api::ApplyBody>()),
            problems: ACTION,
            ..op(
                "post",
                "/v1/sessions/{sid}/checkpoints/{target}/apply",
                "Apply a checkpoint to the real workspace",
                (200, "Apply report", s.out::<api::ApplyResult>()),
            )
        },
        op(
            "get",
            "/v1/sessions/{sid}/diff/{a}/{b}",
            "Changes between two checkpoints or branches",
            (200, "Changes", s.out::<api::Changes>()),
        ),
        Operation {
            auth: Auth::Ingest,
            request: Some(
                json!({ "type": "object", "description": "OTLP/HTTP JSON ExportTraceServiceRequest" }),
            ),
            problems: INGEST,
            ..op(
                "post",
                "/v1/sessions/{sid}/otlp/v1/traces",
                "OTLP/HTTP JSON trace export into a live session",
                (200, "Accepted", s.out::<api::OtlpExportResponse>()),
            )
        },
        Operation {
            auth: Auth::Ingest,
            request: Some(json!({ "type": "object", "description": "The agent's hook payload" })),
            problems: INGEST,
            ..op(
                "post",
                "/v1/sessions/{sid}/ingest/hooks/{adapter}",
                "Hook payload into a live session",
                (
                    200,
                    "The session's response",
                    s.out::<wyrd_adapters::Response>(),
                ),
            )
        },
    ];
    v.sort_by_key(|o| (o.path, o.method));
    v
}

fn content(body: &Body) -> Value {
    match body {
        Body::Json(schema) => json!({ "application/json": { "schema": schema } }),
        Body::Text(mime, desc) => {
            let mut m = Map::new();
            m.insert(
                (*mime).to_string(),
                json!({ "schema": { "type": "string", "description": desc } }),
            );
            Value::Object(m)
        }
    }
}

fn path_params(path: &str) -> Vec<Value> {
    path.split('/')
        .filter_map(|seg| seg.strip_prefix('{')?.strip_suffix('}'))
        .map(|name| json!({ "name": name, "in": "path", "required": true, "schema": { "type": "string" } }))
        .collect()
}

fn query_params(method: &str, path: &str) -> Vec<Value> {
    let q = |name: &str, ty: &str, desc: &str| json!({ "name": name, "in": "query", "required": false, "schema": { "type": ty }, "description": desc });
    match (method, path) {
        ("get", "/v1/sessions") => vec![
            q("status", "string", "Session status"),
            q("agent", "string", "Agent adapter"),
            q("repo", "string", "Workspace path prefix"),
            q(
                "since",
                "integer",
                "Created at or after (ms since the epoch)",
            ),
        ],
        ("get", "/v1/sessions/{sid}/events") => vec![
            q("plane", "string", "Only this plane"),
            q("kind", "string", "Exact kind or dotted prefix"),
            q("after_seq", "integer", "Start after this seq"),
            q("limit", "integer", "Page size (default 500, max 5000)"),
        ],
        ("get", "/v1/sessions/{sid}/graph") => vec![
            q("from_seq", "integer", "First seq kept"),
            q("to_seq", "integer", "Last seq kept"),
        ],
        ("get", "/v1/sessions/{sid}/stream") => vec![q(
            "after_seq",
            "integer",
            "Resume after this seq (Last-Event-ID wins)",
        )],
        _ => Vec::new(),
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        400 => "Malformed request",
        401 => "Missing or invalid bearer token",
        404 => "Not found",
        409 => {
            "Conflict: ambiguous id, workspace changed, session not live, or idempotency key in flight"
        }
        413 => "Body too large",
        415 => "Unsupported media type",
        422 => "Invalid body or reused idempotency key",
        503 => "Not ready",
        _ => "Internal error",
    }
}

/// The documented operations as (method, path).
pub fn routes() -> Vec<(&'static str, &'static str)> {
    operations(&mut Schemas::new())
        .into_iter()
        .map(|o| (o.method, o.path))
        .collect()
}

/// `get /v1/sessions/{sid}/events` → `getSessionsSidEvents`.
pub fn operation_id(method: &str, path: &str) -> String {
    let mut id = method.to_string();
    for seg in path.trim_start_matches("/v1/").split(['/', '-']) {
        let seg = seg.trim_start_matches('{').trim_end_matches('}');
        let mut c = seg.chars();
        if let Some(f) = c.next() {
            id.push(f.to_ascii_uppercase());
            id.extend(c);
        }
    }
    id
}

/// The whole document.
pub fn document() -> Value {
    let mut s = Schemas::new();
    let Body::Json(problem) = s.out::<Problem>() else {
        unreachable!("schemas are JSON")
    };
    let ops = operations(&mut s);
    let mut paths = Map::new();
    for o in &ops {
        let mut responses = Map::new();
        for (status, desc, body) in std::iter::once(&o.ok).chain(&o.also) {
            responses.insert(
                status.to_string(),
                json!({ "description": desc, "content": content(body) }),
            );
        }
        for status in o.problems.iter().chain(&[401, 500]) {
            responses.insert(
                status.to_string(),
                json!({
                    "description": reason(*status),
                    "content": { "application/problem+json": { "schema": problem } }
                }),
            );
        }
        let mut params = path_params(o.path);
        params.extend(query_params(o.method, o.path));
        if o.method == "post" && o.auth == Auth::Daemon {
            params.push(json!({
                "name": "Idempotency-Key", "in": "header", "required": false,
                "schema": { "type": "string", "maxLength": 128 }
            }));
        }
        if o.path.ends_with("/stream") {
            params.push(json!({ "name": "Last-Event-ID", "in": "header", "required": false, "schema": { "type": "string" } }));
        }
        let security = match o.auth {
            Auth::Daemon => json!([{ "daemonToken": [] }, { "unixSocket": [] }]),
            Auth::Ingest => json!([{ "ingestToken": [] }, { "unixSocket": [] }]),
        };
        let mut operation = json!({
            "operationId": operation_id(o.method, o.path),
            "summary": o.summary,
            "parameters": params,
            "responses": responses,
            "security": security,
        });
        if let Some(req) = &o.request {
            operation["requestBody"] = json!({
                "required": false,
                "content": { "application/json": { "schema": req } }
            });
        }
        let entry = paths.entry(o.path.to_string()).or_insert_with(|| json!({}));
        entry[o.method] = operation;
    }
    let mut schemas = s.out.take_definitions(true);
    schemas.extend(s.inp.take_definitions(true));
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Wyrdline daemon API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "wyrdd, served on a same-UID unix socket and on loopback TCP. See docs/architecture/daemon.md."
        },
        "servers": [{ "url": "http://127.0.0.1:7717" }],
        "paths": paths,
        "components": {
            "schemas": schemas,
            "securitySchemes": {
                "daemonToken": {
                    "type": "http", "scheme": "bearer",
                    "description": "The daemon token in $XDG_STATE_HOME/wyrdline/token"
                },
                "ingestToken": {
                    "type": "http", "scheme": "bearer",
                    "description": "Per-session ingest token, derived one-way from the daemon token"
                },
                "unixSocket": {
                    "type": "apiKey", "in": "header", "name": "X-Unix-Socket",
                    "description": "No credential is sent: the unix socket admits only the daemon's own UID (SO_PEERCRED)"
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_ids_are_camel_case() {
        assert_eq!(
            operation_id("get", "/v1/sessions/{sid}/events"),
            "getSessionsSidEvents"
        );
        assert_eq!(
            operation_id("get", "/v1/sessions/{sid}/tool-calls/{tcid}"),
            "getSessionsSidToolCallsTcid"
        );
    }

    #[test]
    fn every_schema_reference_resolves() {
        let doc = document();
        let text = doc.to_string();
        for part in text.split("\"$ref\":\"").skip(1) {
            let r = part.split('"').next().unwrap_or_default();
            let name = r.trim_start_matches("#/components/schemas/");
            assert!(
                !doc["components"]["schemas"][name].is_null(),
                "unresolved {r}"
            );
        }
    }
}
