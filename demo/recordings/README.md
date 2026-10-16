# Recorded sessions

`demo-session.jsonl` is a real recording of the `basic` fakeagent scenario
(`demo/scenarios/`) run through `wyrd run` with the eBPF sensor, exported
with `wyrd events <session> --json`. Machine-specific paths were rewritten
(the workspace to `/home/dev/shop`, build outputs to `/usr/local/bin/`, the
scratch directory to `/tmp/agent`); nothing else was changed.

The landing page ships it as the Studio demo
(`studio/?offline=…/demo-session.jsonl`), where the WebAssembly correlator
rebuilds the causal graph in the browser.
