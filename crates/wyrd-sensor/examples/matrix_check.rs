//! Kernel-matrix check, run as root inside a test VM (or with sudo): mount
//! tracefs and cgroup2 if needed, load and attach every program, run a
//! scripted process tree in a fresh cgroup, and require the golden effect
//! set. Exit code 0 = pass.
#![allow(clippy::print_stdout, clippy::print_stderr, clippy::unwrap_used)]

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use wyrd_schema::EventBody;

fn mount_if_missing(probe: &str, fstype: &str, target: &str) {
    if !Path::new(probe).exists() {
        let _ = std::fs::create_dir_all(target);
        let ok = Command::new("mount")
            .args(["-t", fstype, fstype, target])
            .status()
            .is_ok_and(|s| s.success());
        println!("mounted {fstype} at {target}: {ok}");
    }
}

fn main() {
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    println!("kernel {}", release.trim());
    mount_if_missing(
        "/sys/kernel/tracing/events",
        "tracefs",
        "/sys/kernel/tracing",
    );
    mount_if_missing(
        "/sys/fs/cgroup/cgroup.controllers",
        "cgroup2",
        "/sys/fs/cgroup",
    );

    let (mut sensor, report) = match wyrd_sensor::Sensor::load(&wyrd_sensor::SensorConfig {
        ringbuf_bytes: 4 << 20,
    }) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("FAIL: load: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "attached {} skipped {:?}",
        report.attached.len(),
        report.skipped.iter().map(|(n, _)| n).collect::<Vec<_>>()
    );

    let cg = Path::new("/sys/fs/cgroup/wyrdline-matrix");
    std::fs::create_dir_all(cg).unwrap();
    let id = std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(cg).unwrap());
    sensor
        .register(id, wyrd_sensor::loader::cgroup_level(cg))
        .unwrap();
    let dir = std::env::temp_dir().join(format!("wyrd-matrix-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = format!(
        "echo $$ > {cg}/cgroup.procs; cd {dir}; echo hi > a.txt; mkdir sub; mv a.txt sub/b.txt; chmod 600 sub/b.txt; \
         rm sub/b.txt; rmdir sub; exit 7",
        cg = cg.display(),
        dir = dir.display()
    );
    let status = Command::new("sh").arg("-c").arg(&script).status().unwrap();
    let mut decoder = wyrd_sensor::Decoder::new();
    decoder.set_workspace(id, dir.clone());
    let mut seen: Vec<String> = Vec::new();
    for _ in 0..10 {
        for raw in sensor.poll(Duration::from_millis(100)).unwrap() {
            if let Some(d) = decoder.decode(&raw) {
                let rel = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
                seen.push(match &d.body {
                    EventBody::ProcExec(x) => format!("exec {}", rel(&x.filename)),
                    EventBody::FileOpenWrite(x) if x.file.in_cell => {
                        format!("write {}", rel(&x.file.path))
                    }
                    EventBody::FileMkdir(x) => format!("mkdir {}", rel(&x.path)),
                    EventBody::FileRename(x) => format!("rename {}", rel(&x.to.path)),
                    EventBody::FileChmod(x) => format!("chmod {:o}", x.mode),
                    EventBody::FileUnlink(x) => format!("unlink {}", rel(&x.file.path)),
                    EventBody::ProcExit(x) => format!("exit {}", x.exit_code),
                    other => other.kind().to_string(),
                });
            }
        }
    }
    let _ = std::fs::remove_dir(cg);
    let _ = std::fs::remove_dir_all(&dir);
    println!(
        "shell: {status}; events: {seen:?}; drops: {}",
        sensor.drops().unwrap_or(0)
    );
    let want = [
        "write a.txt",
        "exec mkdir",
        "mkdir sub",
        "exec mv",
        "rename b.txt",
        "chmod 600",
        "exec rm",
        "unlink b.txt",
        "unlink sub",
        "exit 7",
    ];
    let missing: Vec<&str> = want
        .iter()
        .copied()
        .filter(|w| !seen.iter().any(|s| s == w))
        .collect();
    if missing.is_empty() && status.code() == Some(7) {
        println!("PASS");
    } else {
        eprintln!("FAIL: missing {missing:?}");
        std::process::exit(1);
    }
}
