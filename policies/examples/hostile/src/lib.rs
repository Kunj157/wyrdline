//! A policy that misbehaves on request, to test the host's limits. The tool
//! name picks the behaviour; anything else is allowed.

#![allow(clippy::too_many_arguments)] // generated bindings

wit_bindgen::generate!({ path: "../../wit", world: "policy" });

use std::hint::black_box;

struct Hostile;

impl Guest for Hostile {
    fn kernel_rules(ctx: SessionCtx) -> KernelRules {
        if ctx.repo_root == "spin" {
            spin();
        }
        KernelRules {
            fs_read_deny: vec!["~/.ssh".into()],
            fs_write_allow: vec![ctx.repo_root],
            exec_deny: vec!["curl".into()],
            egress_allow_cidrs: vec!["10.0.0.0/8".into()],
            egress_allow_hosts: vec!["example.org".into()],
            egress_allow_ports: vec![443],
        }
    }

    fn on_tool_call(call: ToolCall, ctx: SessionCtx) -> Decision {
        match call.tool.as_str() {
            "spin" => spin(),
            "hog" => {
                // Far beyond the host's memory cap.
                let v = black_box(vec![1u8; 256 << 20]);
                Decision::Deny(format!("{}", v[v.len() - 1]))
            }
            "panic" => panic!("policy panicked"),
            "clock" => {
                // Needs a WASI clock import, which the host traps.
                let t = std::time::SystemTime::now();
                Decision::Deny(format!("{t:?}"))
            }
            "flood" => {
                for i in 0..10_000 {
                    log(&format!("line {i} {}", "x".repeat(2_000)));
                }
                Decision::Allow
            }
            "deny" => Decision::Deny(format!("denied in {}", ctx.repo_root)),
            "ask" => Decision::Ask(format!("args {}", call.args_json)),
            _ => {
                log(&format!("allow {}", call.tool));
                Decision::Allow
            }
        }
    }

    fn on_effect(effect: Effect, _ctx: SessionCtx) -> Decision {
        match effect.kind.as_str() {
            "exec" if effect.exe.as_deref() == Some("/usr/bin/curl") => {
                Decision::Deny("curl".into())
            }
            "net.connect" => Decision::Ask(format!(
                "{}:{}",
                effect.host.unwrap_or_default(),
                effect.port.unwrap_or_default()
            )),
            _ => Decision::Allow,
        }
    }
}

fn spin() -> ! {
    let mut i = 0u64;
    loop {
        i = black_box(i.wrapping_add(1));
    }
}

export!(Hostile);
