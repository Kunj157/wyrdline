//! Manual smoke test (root): load the sensor, put a child shell into a
//! fresh cgroup, run a few file and network operations, print the events.
//!
//! sudo target/debug/examples/smoke
#![allow(clippy::print_stdout, clippy::unwrap_used)]

use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (mut sensor, report) = wyrd_sensor::Sensor::load(&wyrd_sensor::SensorConfig {
        ringbuf_bytes: 4 << 20,
    })?;
    println!(
        "attached {} programs, skipped {:?}",
        report.attached.len(),
        report.skipped
    );
    let cg = std::path::Path::new("/sys/fs/cgroup/wyrdline-smoke");
    std::fs::create_dir_all(cg)?;
    let id = std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(cg)?);
    sensor.register(id, 1)?;
    let dir = tempfile_dir();
    let script = format!(
        "echo $$ > {cg}/cgroup.procs; cd {dir}; echo hi > a.txt; mkdir sub; mv a.txt sub/b.txt; chmod 600 sub/b.txt; \
         truncate -s 0 sub/b.txt; rm sub/b.txt; rmdir sub; cat /etc/hostname > /dev/null; \
         (exec 3<>/dev/tcp/127.0.0.1/9 ) 2>/dev/null; exit 7",
        cg = cg.display(),
        dir = dir
    );
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(&script)
        .status()?;
    println!("shell exit: {status}");
    let mut dec = wyrd_sensor::Decoder::new();
    dec.set_workspace(id, dir.clone().into());
    let mut total = 0;
    for _ in 0..5 {
        for raw in sensor.poll(Duration::from_millis(200))? {
            total += 1;
            if let Some(d) = dec.decode(&raw) {
                println!(
                    "{:>7} {:<28} {}",
                    d.process.pid,
                    d.body.kind(),
                    serde_json_lite(&d.body)
                );
            }
        }
    }
    println!("raw events: {total}, drops: {}", sensor.drops()?);
    let _ = std::fs::remove_dir(cg);
    let _ = std::fs::remove_dir(&dir);
    Ok(())
}

fn tempfile_dir() -> String {
    let d = std::env::temp_dir().join(format!("wyrd-smoke-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d.display().to_string()
}

fn serde_json_lite(b: &wyrd_schema::EventBody) -> String {
    let s = format!("{b:?}");
    s.chars().take(150).collect()
}
