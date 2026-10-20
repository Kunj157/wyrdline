#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use proptest::prelude::*;
use wyrd_schema::*;

fn s() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9_./ -]{0,24}"
}

fn path() -> impl Strategy<Value = FilePath> {
    (s(), any::<bool>(), any::<bool>()).prop_map(|(path, in_cell, truncated)| FilePath {
        path,
        in_cell,
        truncated,
    })
}

fn json() -> impl Strategy<Value = serde_json::Value> {
    let leaf = prop_oneof![
        Just(serde_json::Value::Null),
        any::<bool>().prop_map(serde_json::Value::from),
        any::<i64>().prop_map(serde_json::Value::from),
        s().prop_map(serde_json::Value::from),
    ];
    leaf.prop_recursive(3, 16, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(serde_json::Value::from),
            prop::collection::btree_map("[a-z]{1,6}", inner, 0..4)
                .prop_map(|m| serde_json::Value::Object(m.into_iter().collect())),
        ]
    })
}

#[allow(clippy::too_many_lines)]
fn body() -> impl Strategy<Value = EventBody> {
    prop_oneof![
        (s(), s(), prop::collection::vec(s(), 0..3), any::<bool>()).prop_map(
            |(session_id, agent_kind, command, cell)| {
                let mut capabilities = BTreeMap::new();
                capabilities.insert("btf".to_string(), cell);
                EventBody::SessionStarted(SessionStarted {
                    session_id,
                    agent_kind,
                    agent_version: None,
                    command,
                    workspace_root: "/w".into(),
                    mode: if cell {
                        CaptureMode::Cell
                    } else {
                        CaptureMode::Observe
                    },
                    kernel_release: "6.17.0".into(),
                    capabilities,
                    policy_set_hash: None,
                })
            }
        ),
        (s(), any::<Option<i32>>()).prop_map(|(reason, exit_code)| EventBody::SessionClosed(
            SessionClosed { reason, exit_code }
        )),
        any::<u64>().prop_map(|dropped| EventBody::SensorResumed(SensorResumed { dropped })),
        (s(), s(), json(), any::<Option<u32>>()).prop_map(|(call_id, tool, args, turn)| {
            EventBody::ToolCallStarted(ToolCallStarted {
                call_id,
                source: IntentSource::Hook,
                tool,
                args,
                turn,
            })
        }),
        (s(), any::<Option<i32>>(), any::<u64>()).prop_map(|(call_id, exit_code, duration_ms)| {
            EventBody::ToolCallFinished(ToolCallFinished {
                call_id,
                status: CallStatus::Ok,
                exit_code,
                summary: None,
                duration_ms,
            })
        }),
        (any::<u32>(), s()).prop_map(|(turn, text)| EventBody::Message(Message {
            role: Role::Assistant,
            turn,
            text
        })),
        (s(), s(), json()).prop_map(
            |(server, method, params)| EventBody::McpRequest(McpRequest {
                server,
                request_id: "1".into(),
                method,
                params
            })
        ),
        (s(), prop::collection::vec(s(), 0..4), any::<bool>()).prop_map(
            |(filename, argv, truncated)| EventBody::ProcExec(ProcExec {
                filename,
                argv,
                cwd: None,
                truncated
            })
        ),
        (any::<u32>(), any::<u64>()).prop_map(|(child_pid, child_start_ns)| EventBody::ProcFork(
            ProcFork {
                child_pid,
                child_start_ns
            }
        )),
        (any::<i32>(), any::<Option<i32>>())
            .prop_map(|(exit_code, signal)| EventBody::ProcExit(ProcExit { exit_code, signal })),
        (path(), any::<u64>())
            .prop_map(|(file, bytes)| EventBody::FileWrite(FileWrite { file, bytes })),
        (path(), path()).prop_map(|(from, to)| EventBody::FileRename(FileRename { from, to })),
        (path(), any::<bool>())
            .prop_map(|(file, is_dir)| EventBody::FileUnlink(FileUnlink { file, is_dir })),
        path().prop_map(EventBody::FileReadSensitive),
        (s(), any::<u16>(), any::<bool>()).prop_map(|(addr, port, ok)| EventBody::NetConnect(
            NetConnect {
                family: 4,
                addr,
                port,
                dns_name: None,
                verdict: if ok {
                    NetVerdict::Allowed
                } else {
                    NetVerdict::Denied
                },
            }
        )),
        (s(), s(), any::<u64>()).prop_map(|(checkpoint_id, manifest, bytes_added)| {
            EventBody::CheckpointCreated(CheckpointCreated {
                checkpoint_id,
                branch_id: "a".into(),
                parent_id: None,
                after_call_id: None,
                manifest,
                files_changed: 1,
                bytes_added,
                duration_ms: 3,
            })
        }),
        (s(), any::<Option<u64>>()).prop_map(|(reason, subject_seq)| EventBody::GuardDecision(
            GuardDecision {
                stage: GuardStage::Kernel,
                decision: Decision::Deny,
                policy_id: "default".into(),
                policy_version: "1".into(),
                reason,
                call_id: None,
                subject_seq,
            }
        )),
        (
            s(),
            prop::collection::vec(any::<u64>(), 0..5),
            0u16..=10_000
        )
            .prop_map(|(summary, evidence_seqs, confidence_bp)| {
                EventBody::DivergenceRaised(DivergenceRaised {
                    divergence_id: "d1".into(),
                    rule_id: "D001".into(),
                    severity: Severity::High,
                    claim_id: None,
                    evidence_seqs,
                    confidence_bp,
                    summary,
                })
            }),
        (s(), s()).prop_map(|(checkpoint_id, reason)| EventBody::RewindRequested(
            RewindRequested {
                checkpoint_id,
                reason,
                source: "mcp".into(),
            }
        )),
        (any::<u64>(), s()).prop_map(|(request_seq, checkpoint_id)| {
            EventBody::RewindConfirmed(RewindConfirmed {
                request_seq,
                checkpoint_id,
                by: "cli".into(),
            })
        }),
        (any::<u64>(), any::<Option<String>>()).prop_map(|(request_seq, note)| {
            EventBody::RewindRejected(RewindRejected {
                request_seq,
                by: "studio".into(),
                note,
            })
        }),
    ]
}

fn event() -> impl Strategy<Value = Event> {
    (
        any::<u64>(),
        any::<u64>(),
        any::<i64>(),
        any::<Option<(u32, u64)>>(),
        any::<u32>(),
        body(),
    )
        .prop_map(
            |(seq, ts_mono_ns, ts_wall_ms, proc_, drops_since_last, body)| Event {
                seq,
                ts_mono_ns,
                ts_wall_ms,
                process: proc_.map(|(pid, start_ns)| ProcessRef {
                    pid,
                    tid: pid,
                    ppid: 1,
                    start_ns,
                    cgroup_id: 42,
                }),
                drops_since_last,
                body,
            },
        )
}

proptest! {
    #[test]
    fn cbor_round_trip(ev in event()) {
        let bytes = to_canonical_cbor(&ev).unwrap();
        prop_assert_eq!(from_canonical_cbor(&bytes).unwrap(), ev);
    }

    #[test]
    fn json_round_trip(ev in event()) {
        let text = serde_json::to_string(&ev).unwrap();
        prop_assert_eq!(serde_json::from_str::<Event>(&text).unwrap(), ev);
    }

    #[test]
    fn encoding_is_deterministic(ev in event()) {
        let a = to_canonical_cbor(&ev).unwrap();
        let b = to_canonical_cbor(&ev.clone()).unwrap();
        prop_assert_eq!(a, b);
    }

    #[test]
    fn decoder_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let _ = from_canonical_cbor(&bytes);
    }
}

#[test]
fn kind_names_match_serialised_tags_and_planes() {
    let ev = Event {
        seq: 1,
        ts_mono_ns: 0,
        ts_wall_ms: 0,
        process: None,
        drops_since_last: 0,
        body: EventBody::FileMkdir(FilePath {
            path: "a".into(),
            in_cell: true,
            truncated: false,
        }),
    };
    let v = serde_json::to_value(&ev).unwrap();
    assert_eq!(v["body"]["kind"], "effect.file.mkdir");
    assert_eq!(ev.kind(), "effect.file.mkdir");
    assert_eq!(ev.plane(), Plane::Effect);
    for (kind, plane) in KINDS {
        assert!(
            kind.starts_with(plane.as_str()),
            "{kind} not in plane {plane:?}"
        );
    }
    let mut unique: Vec<_> = KINDS.iter().map(|(k, _)| *k).collect();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), KINDS.len());
}

#[test]
fn rewind_requests_are_meta_events() {
    let body = EventBody::RewindRequested(RewindRequested {
        checkpoint_id: "c2".into(),
        reason: "the last edit broke the build".into(),
        source: "mcp".into(),
    });
    assert_eq!(body.kind(), "meta.rewind.requested");
    assert_eq!(body.plane(), Plane::Meta);
    let v = serde_json::to_value(&body).unwrap();
    assert_eq!(v["kind"], "meta.rewind.requested");
    assert_eq!(v["payload"]["checkpoint_id"], "c2");
    let confirmed = EventBody::RewindConfirmed(RewindConfirmed {
        request_seq: 7,
        checkpoint_id: "c2".into(),
        by: "cli".into(),
    });
    assert_eq!(confirmed.kind(), "meta.rewind.confirmed");
    let rejected = EventBody::RewindRejected(RewindRejected {
        request_seq: 7,
        by: "studio".into(),
        note: None,
    });
    assert_eq!(rejected.kind(), "meta.rewind.rejected");
    for k in [
        "meta.rewind.requested",
        "meta.rewind.confirmed",
        "meta.rewind.rejected",
    ] {
        assert!(
            KINDS
                .iter()
                .any(|(kind, p)| *kind == k && *p == Plane::Meta)
        );
    }
}

#[test]
fn unknown_kind_is_rejected() {
    let text = r#"{"seq":1,"ts_mono_ns":0,"ts_wall_ms":0,"process":null,"drops_since_last":0,
        "body":{"kind":"effect.teleport","payload":{}}}"#;
    assert!(serde_json::from_str::<Event>(text).is_err());
}

#[test]
fn unknown_fields_are_rejected() {
    let text = r#"{"seq":1,"ts_mono_ns":0,"ts_wall_ms":0,"process":null,"drops_since_last":0,
        "extra":1,"body":{"kind":"effect.net.dns_query","payload":{"name":"x"}}}"#;
    assert!(serde_json::from_str::<Event>(text).is_err());
}

#[test]
fn oversized_input_is_rejected() {
    let big = vec![0u8; codec_limit() + 1];
    assert!(matches!(
        from_canonical_cbor(&big),
        Err(CodecError::TooLarge(_))
    ));
}

fn codec_limit() -> usize {
    1024 * 1024
}
