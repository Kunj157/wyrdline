# Installing Wyrdline

Wyrdline is three programs:

| Program | Runs as | Needed for |
|---|---|---|
| `wyrd` | you | everything: `wyrd doctor`, `wyrd run`, inspecting and exporting sessions |
| `wyrd-sensord` | root (systemd service, bounded capabilities) | kernel effects: what the agent's processes actually exec, write, delete, rename, and connect to; kernel egress enforcement |
| `wyrdd` | you | the HTTP API and **Studio**, the web UI |

Without `wyrd-sensord`, `wyrd run` still sandboxes the agent, records its
tool calls, and checkpoints the workspace; the session is marked
*Degraded* because kernel effects are missing
([limitations](limitations.md)). Without `wyrdd` there is no Studio,
but every CLI command works.

Linux only, x86_64 or aarch64 ([requirements](#kernel-requirements)).

## From a release

Each GitHub release carries, for every architecture:

```
wyrdline-<arch>-unknown-linux-gnu.tar.gz    wyrdline/bin/{wyrd,wyrdd,wyrd-sensord,wyrd-hub},
                                            wyrdline/share/wyrdline/studio, deploy/ (sensor unit, AppArmor profile)
wyrdline_<version>_<amd64|arm64>.deb         /usr/bin binaries, the sensor unit and AppArmor profile, the Studio
wyrdline-<binary>-<target>.cdx.json          CycloneDX SBOM per binary and target
SHA256SUMS                                   sha256sum lines for every asset
SHA256SUMS.sigstore.json                     cosign keyless signature over SHA256SUMS
```

`<arch>` is `x86_64` or `aarch64`. Each asset also has a build-provenance
attestation. The GitHub Action's installer relies on the archive and
`SHA256SUMS` (`integrations/github-action/scripts/install.sh`); a release
that lacks `SHA256SUMS` is refused by it. The `.deb` does not enable the
sensor; the step below does.

The Hub's container image and Helm chart are described in [the Hub](hub.md).

### 1. Download and verify

```sh
v=v1.0.0                                # the release tag
base=https://github.com/Kunj157/wyrdline/releases/download/$v
curl -fsSLO $base/wyrdline-$(uname -m)-unknown-linux-gnu.tar.gz
curl -fsSLO $base/SHA256SUMS
curl -fsSLO $base/SHA256SUMS.sigstore.json

# The signature: SHA256SUMS was signed by the release workflow of this
# repository, on a tag, through GitHub's OIDC identity (no key to manage).
cosign verify-blob \
  --bundle SHA256SUMS.sigstore.json \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-identity-regexp '^https://github\.com/Kunj157/wyrdline/\.github/workflows/release\.yml@refs/tags/v' \
  SHA256SUMS

# The checksum of what you downloaded.
sha256sum --check --ignore-missing SHA256SUMS
```

`cosign` is available from <https://github.com/sigstore/cosign>. Verify
before you install: `wyrd-sensord` runs as root.

### 2. Install the binaries

```sh
tar -xzf wyrdline-$(uname -m)-unknown-linux-gnu.tar.gz
sudo install -m 0755 wyrdline/bin/wyrd wyrdline/bin/wyrdd wyrdline/bin/wyrd-sensord wyrdline/bin/wyrd-hub /usr/local/bin/
sudo cp -R wyrdline/share/wyrdline /usr/local/share/
wyrd --version
```

`wyrd` must be on `PATH` for the agent too (the agent's hooks run
`wyrd hook …`). `wyrdd` looks for the built Studio at
`<prefix>/share/wyrdline/studio` next to its own `<prefix>/bin/wyrdd`,
which is what the last command sets up for `/usr/local`; see
[Studio](#studio) for other layouts.

### 3. Install the sensor

The sensor is a systemd service. Its unit file is
[`deploy/systemd/wyrd-sensord.service`](../deploy/systemd/wyrd-sensord.service):
it starts `/usr/local/bin/wyrd-sensord --socket /run/wyrdline/sensord.sock`
with only the capabilities loading tracepoint programs needs
(`CAP_BPF CAP_PERFMON CAP_SYS_RESOURCE CAP_DAC_READ_SEARCH CAP_SYS_ADMIN`),
no network (`PrivateNetwork=yes`, `AF_UNIX` only), a read-only system,
and no access to home directories.

```sh
curl -fsSLO https://raw.githubusercontent.com/Kunj157/wyrdline/$v/deploy/systemd/wyrd-sensord.service
sudo install -m 0644 wyrd-sensord.service /etc/systemd/system/wyrd-sensord.service
sudo systemctl daemon-reload
sudo systemctl enable --now wyrd-sensord.service
wyrd doctor                     # "wyrd-sensord  listening at /run/wyrdline/sensord.sock"
```

The socket is world-connectable; authorisation is per request: a user
can only register cgroups they own (`SO_PEERCRED`), so one user never
sees another user's sessions ([sensor](architecture/sensor.md)).

`wyrd-sensord --ringbuf-mb N` sizes the ring buffer (default 64 MiB,
a power of two); drops are counted in the ledger either way.

### Using the Action's installer outside CI

`integrations/github-action/scripts/install.sh` does steps 1 and 2 for
`wyrd` and `wyrd-sensord` (it needs `curl`, `tar`, `sha256sum`, and
`cosign` when the release is signed):

```sh
WYRD_VERSION=v1.0.0 WYRD_INSTALL_DIR=$HOME/.local/bin \
  bash integrations/github-action/scripts/install.sh
```

| Variable | Default | Meaning |
|---|---|---|
| `WYRD_VERSION` | `latest` | release tag |
| `WYRD_REPOSITORY` | `Kunj157/wyrdline` | repository to download from |
| `WYRD_INSTALL_DIR` | `$RUNNER_TEMP/wyrdline/bin` (or `/tmp/wyrdline/bin`) | where the binaries go |
| `WYRD_VERIFY_SIGNATURE` | `auto` | `auto`: verify when `SHA256SUMS.sigstore.json` exists; `required`: fail without it; `skip` |
| `WYRD_SIGNER_IDENTITY` | the release workflow of `WYRD_REPOSITORY` on a `v*` tag | regexp for `cosign --certificate-identity-regexp` |
| `WYRD_PATH` | unset | a directory with prebuilt `wyrd` and `wyrd-sensord`: install those, download nothing |

The checksum is always verified. In a GitHub job the script also appends
the directory to `GITHUB_PATH`.

### `.deb`

Each release ships a Debian package; its name and contents are listed in
the release notes of that version. Verify its
checksum against the signed `SHA256SUMS` as above before
`sudo dpkg -i <file>.deb`, then check `wyrd doctor`: if the package
did not enable the sensor, finish with step 3.

## From source

Toolchain, as pinned in the repository:

- Rust `1.99.0` with the `wasm32-unknown-unknown` and `wasm32-wasip2`
  targets ([`rust-toolchain.toml`](../rust-toolchain.toml); `rustup`
  installs it on first use in the checkout), plus a **nightly**
  toolchain with `rust-src` and `bpf-linker` for the eBPF object:
  `rustup toolchain install nightly --profile minimal --component rust-src`
  and `cargo install --locked bpf-linker`.
- Node 22 ([`.node-version`](../.node-version)) and pnpm 12
  (`packageManager` in [`package.json`](../package.json)) for Studio.
- `wasm-bindgen-cli` 0.2.129 for Studio's offline mode (the correlator
  compiled to WebAssembly).
- `cargo-nextest` to run the tests.

```sh
git clone https://github.com/Kunj157/wyrdline && cd wyrdline
make build                       # release crates, then Studio (pnpm install --frozen-lockfile first: make install)
make policies                    # policies/default.wasm and policies/strict.wasm
sudo install -m 0755 target/release/wyrd target/release/wyrdd /usr/local/bin/
sudo make sensord-install        # builds wyrd-sensord, installs the binary and the unit, starts it
```

`make build` runs `cargo build --workspace --release --locked` and
`pnpm build`; the binaries are in `target/release/`
(`wyrd`, `wyrdd`, `wyrd-sensord`, `wyrd-hub`, `wyrd-fakeagent`) and
Studio in `apps/studio/dist`. `scripts/build-studio-wasm.sh` builds the
offline correlator into the Studio before `pnpm build`
(`make studio-wasm`). `make help` lists every target.

## Studio

`wyrdd` serves Studio on `127.0.0.1:7717` from the first of:

1. `wyrdd --studio <dir>`;
2. `$WYRD_STUDIO_DIR`;
3. `share/wyrdline/studio` next to the installed binary
   (`<prefix>/bin/wyrdd` → `<prefix>/share/wyrdline/studio`).

Without an `index.html` there it serves the API only and logs
"no Studio build found". From a source checkout:

```sh
WYRD_STUDIO_DIR=$PWD/apps/studio/dist wyrdd
wyrd studio                      # opens http://127.0.0.1:7717/#token=…
```

The daemon needs no root and no configuration: it listens on a unix
socket for your UID only and on loopback with a bearer token it creates
on first start ([daemon](architecture/daemon.md)). `wyrd studio --print`
prints the URL instead of opening a browser.

## Kernel requirements

`wyrd doctor` runs every check below and prints what degrades without
it; exit code 2 when Cell mode is not possible.

| Check (`wyrd doctor` id) | Needs | Without it |
|---|---|---|
| `kernel` | Linux ≥ 5.15 (the sensor's kernel matrix runs on 5.15, 6.1, 6.6, 6.12, 7.0, 7.2 and the Ubuntu 24.04 and 26.04 runner kernels) | older kernels are untested |
| `btf` | `/sys/kernel/btf/vmlinux` | the sensor cannot load: no kernel effects, intent only |
| `ringbuf` | kernel ≥ 5.8 | the sensor cannot stream effects |
| `cgroup_v2` | cgroup v2 mounted at `/sys/fs/cgroup` | **no Cell mode**: sessions cannot be scoped, frozen, or network-filtered |
| `cgroup_delegation` | a systemd user manager (`user@<uid>.service`) | the Cell has no cgroup of its own, so the sensor cannot scope effects to it |
| `landlock` | Landlock in the active LSMs, ABI ≥ 4 for TCP port rules | **no Cell mode**: file access cannot be confined; observe mode only |
| `bpf_lsm` | `bpf` in the active LSMs (optional) | exec denial stays detect-and-stop; file checks rely on Landlock |
| `userns` | unprivileged user namespaces | the Cell cannot be created directly; see the two ways below |
| `overlay_userns` | overlayfs, kernel ≥ 5.11 | **no Cell mode**: no checkpoints, rewind, or fork |
| `cgroup_sock_addr` | kernel ≥ 4.17 and cgroup v2 | egress cannot be filtered in the kernel |
| `sensord` | `wyrd-sensord` listening at `/run/wyrdline/sensord.sock` | only intent (hooks, MCP, OTLP) is captured, not kernel effects |

The summary line says what the machine supports:
`Cell mode: yes   Kernel capture: yes   Kernel network enforcement: yes`.
A stock Ubuntu 24.04 gets there with the sensor and the AppArmor profile
below installed; the optional BPF LSM is usually not enabled.

**User namespaces on Ubuntu 23.10 and later.** AppArmor restricts
unprivileged user namespaces (`kernel.apparmor_restrict_unprivileged_userns = 1`),
and `wyrd doctor` says so. Either install the AppArmor profile that
grants `userns` to the `wyrd` binary and nothing else:

```sh
sudo make apparmor-install WYRD_BIN=$(command -v wyrd)   # from a checkout; installs deploy/apparmor/wyrd
```

or let `wyrd run` go through `sudo` for the two internal commands that
set up the mount namespace, dropping back to your user before the agent
starts: `export WYRD_PRIVILEGED_HELPER=sudo` (passwordless sudo for
`wyrd __cell-exec` and `wyrd __cell-cleanup` is enough). Details in
[the Cell](architecture/cell.md).

## Where things live

| What | Path |
|---|---|
| sessions, CAS, host signing key, uploaded policies | `$WYRD_DATA_DIR`, else `$XDG_DATA_HOME/wyrdline`, else `~/.local/share/wyrdline` (`sessions/<id>/ledger.wyrdledger`, `cas/`, `keys/host.key`, `policies/`) |
| daemon token | `$XDG_STATE_HOME/wyrdline/token`, else `~/.local/state/wyrdline/token` (`wyrd token path`) |
| daemon socket | `$WYRD_DAEMON_SOCKET`, else `$XDG_RUNTIME_DIR/wyrdline/wyrdd.sock` |
| intent socket of a running session | `$XDG_RUNTIME_DIR/wyrdline/intent-<session>.sock` |
| sensor socket | `$WYRD_SENSORD_SOCKET`, else `/run/wyrdline/sensord.sock` |
| project configuration | `.wyrdline/config.toml`, `scope.toml`, `secrets.toml` in the workspace ([policy guide](policies/README.md)) |

## Environment variables

| Variable | Read by | Meaning |
|---|---|---|
| `WYRD_DATA_DIR` | `wyrd`, `wyrdd` | data directory (above) |
| `WYRD_SENSORD_SOCKET` | `wyrd` | sensor socket |
| `WYRD_DAEMON_SOCKET` | `wyrd`, `wyrdd` | daemon unix socket |
| `WYRD_DAEMON_LISTEN` | `wyrd run` | the daemon's loopback address when it is not `127.0.0.1:7717` (for the OTLP endpoint handed to the agent) |
| `WYRD_STUDIO_DIR` | `wyrdd` | built Studio to serve |
| `WYRD_STUDIO_ORIGIN` | `wyrdd` | one extra CORS origin (Studio development server) |
| `WYRD_LOG` | `wyrdd`, `wyrd-hub` | log filter (`info` by default; JSON lines on stderr) |
| `WYRD_PRIVILEGED_HELPER` | `wyrd run` | `sudo`: set up the Cell's namespaces through `sudo -n` |
| `WYRD_DOCTOR_MASK` | `wyrd doctor`, `wyrd run` | comma-separated check ids to pretend are missing (testing) |
| `ANTHROPIC_API_KEY`, `LLM_PROVIDER`, `ANTHROPIC_BASE_URL`, `WYRD_CLAIM_MODEL`, `WYRD_LLM_MAX_COST_USD` | `wyrd run`, `wyrd divergences` | claim extraction with a model: on when the key is set (`LLM_PROVIDER=none` turns it off); model `claude-haiku-4-5` and a `0.50` USD cap per session by default |
| `OTEL_EXPORTER_OTLP_ENDPOINT` and friends | the agent | if you set an OTLP endpoint yourself, `wyrd run` leaves it alone; otherwise it points the agent's trace exporter at the daemon |
| `WYRD_INTENT_SOCKET`, `WYRD_MODE`, `WYRD_BIN` | the agent, set by `wyrd run` | how hooks reach the session; outside a session `wyrd hook` is a no-op |

The Hub's `WYRD_HUB_*` variables are in [the Hub](architecture/hub.md).

Next: the [quick start](quick-start.md).
