//! Builds the eBPF object (`wyrd-ebpf`) for `bpfel-unknown-none` with the
//! nightly toolchain and `bpf-linker`; the result is embedded by `lib.rs`.

fn main() -> anyhow::Result<()> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../wyrd-ebpf");
    let root = root
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-UTF-8 path"))?;
    aya_build::build_ebpf(
        [aya_build::Package {
            name: "wyrd-ebpf",
            root_dir: root,
            ..Default::default()
        }],
        aya_build::Toolchain::default(),
    )
}
