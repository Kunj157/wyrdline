//! Kernel rules only: what the privileged tests and the escape suite
//! (`tests/security/escape`) try to get around. Tool calls and effects are
//! allowed, so every denial in those tests comes from the kernel or the
//! exec deny list.
#![allow(clippy::too_many_arguments)] // generated bindings

wit_bindgen::generate!({ path: "../../wit", world: "policy" });

struct Escape;

impl Guest for Escape {
    fn kernel_rules(_ctx: SessionCtx) -> KernelRules {
        KernelRules {
            fs_read_deny: vec!["~/.ssh".into()],
            fs_write_allow: Vec::new(),
            exec_deny: vec!["curl".into()],
            egress_allow_cidrs: vec!["10.0.0.0/8".into(), "127.0.0.0/8".into()],
            egress_allow_hosts: Vec::new(),
            egress_allow_ports: vec![443],
        }
    }

    fn on_tool_call(_call: ToolCall, _ctx: SessionCtx) -> Decision {
        Decision::Allow
    }

    fn on_effect(_effect: Effect, _ctx: SessionCtx) -> Decision {
        Decision::Allow
    }
}

export!(Escape);
