//! Wyrdline strict policy: everything the default policy asks about is
//! denied, egress is limited to the model API, network tools may not run
//! at all, writes must match the task scope, and reading a secret or
//! executing a network tool stops the session. The rules live in
//! `policy-common`.
#![allow(clippy::too_many_arguments)] // generated bindings

policy_common::policy_component!(policy_common::Profile::Strict);
