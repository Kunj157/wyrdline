//! Temporary diagnostic (not for merge): load the sensor, drive a
//! concurrent workload in a registered cgroup while unrelated CPU and
//! syscall noise runs outside it, then compare what was captured with what
//! was done, and dump drops and per-program kernel counters.
#![allow(clippy::print_stdout, clippy::print_stderr, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use wyrd_schema::EventBody;

fn run_phase(label: &str, workers: usize, iters: usize, noise: usize, extra: usize) {
    let others: Vec<_> = (0..extra)
        .map(|_| {
            wyrd_sensor::Sensor::load(&wyrd_sensor::SensorConfig {
                ringbuf_bytes: 4 << 20,
            })
            .unwrap()
        })
        .collect();
    let (mut sensor, report) = wyrd_sensor::Sensor::load(&wyrd_sensor::SensorConfig {
        ringbuf_bytes: 64 << 20,
    })
    .unwrap();
    println!(
        "[{label}] attached {} skipped {:?}",
        report.attached.len(),
        report.skipped
    );
    let _stats = aya::sys::enable_stats(aya::sys::Stats::RunTime).ok();
    let cg = Path::new("/sys/fs/cgroup/wyrd-diag");
    let _ = std::fs::create_dir_all(cg);
    let id = std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(cg).unwrap());
    sensor
        .register(id, wyrd_sensor::loader::cgroup_level(cg))
        .unwrap();
    let dir = std::env::temp_dir().join(format!("wyrd-diag-{label}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut noise_kids: Vec<Child> = (0..noise)
        .map(|i| {
            let script = if i % 2 == 0 {
                "while :; do :; done".to_string()
            } else {
                "while :; do ls /usr/bin > /dev/null; cat /proc/self/status > /dev/null; done"
                    .to_string()
            };
            Command::new("sh").arg("-c").arg(script).spawn().unwrap()
        })
        .collect();

    let started = Instant::now();
    let kids: Vec<Child> = (0..workers)
        .map(|w| {
            let script = format!(
                "echo $$ > {cg}/cgroup.procs; mkdir -p {dir}/w{w}; cd {dir}/w{w}; \
                 i=0; while [ $i -lt {iters} ]; do echo x > f$i; mkdir d$i; mv d$i e$i; rmdir e$i; rm f$i; i=$((i+1)); done; exit 3",
                cg = cg.display(),
                dir = dir.display(),
            );
            Command::new("sh").arg("-c").arg(script).spawn().unwrap()
        })
        .collect();
    let mut decoder = wyrd_sensor::Decoder::new();
    decoder.set_workspace(id, dir.clone());
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut kids = kids;
    let mut done_at: Option<Instant> = None;
    loop {
        for raw in sensor.poll(Duration::from_millis(10)).unwrap() {
            if let Some(d) = decoder.decode(&raw) {
                let k = match &d.body {
                    EventBody::ProcExec(x) => {
                        format!("exec {}", x.filename.rsplit('/').next().unwrap_or(""))
                    }
                    EventBody::ProcExit(x) => format!("exit {}", x.exit_code),
                    other => other.kind().to_string(),
                };
                *counts.entry(k).or_default() += 1;
            }
        }
        if done_at.is_none() && kids.iter_mut().all(|k| k.try_wait().unwrap().is_some()) {
            done_at = Some(Instant::now());
        }
        if done_at.is_some_and(|t| t.elapsed() > Duration::from_secs(1)) {
            break;
        }
    }
    let elapsed = started.elapsed();
    for k in &mut noise_kids {
        let _ = k.kill();
        let _ = k.wait();
    }
    let n = (workers * iters) as u64;
    println!(
        "[{label}] workers={workers} iters={iters} noise={noise} extra_sensors={extra} elapsed={elapsed:?}"
    );
    println!(
        "[{label}] expected each of exec mkdir/mv/rmdir/rm, mkdir, rename, unlink: {n} (unlink 2x: {})",
        2 * n
    );
    for (k, v) in &counts {
        println!("[{label}]   {k}: {v}");
    }
    println!("[{label}] ringbuf drops: {}", sensor.drops().unwrap_or(0));
    let mut stats = sensor.program_stats();
    stats.sort_by(|a, b| b.misses.cmp(&a.misses));
    for s in stats.iter().filter(|s| s.run_cnt > 0 || s.misses > 0) {
        println!(
            "[{label}]   prog {:<24} run_cnt {:>10} recursion_misses {:>8}",
            s.name, s.run_cnt, s.misses
        );
    }
    for (o, _) in &others {
        let m: u64 = o.program_stats().iter().map(|s| s.misses).sum();
        println!("[{label}]   other sensor misses {m}");
    }
    drop(others);
    let _ = sensor.unregister(id);
    drop(sensor);
    let _ = std::fs::remove_dir(cg);
    let _ = std::fs::remove_dir_all(&dir);
}

fn main() {
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let cpus = std::thread::available_parallelism().map_or(2, std::num::NonZero::get);
    println!("kernel {} cpus {cpus}", release.trim());
    run_phase("quiet", 1, 50, 0, 0);
    run_phase("parallel", cpus * 2, 50, 0, 0);
    run_phase("noisy", cpus * 2, 50, cpus * 2, 0);
    run_phase("multi", cpus * 2, 50, cpus * 2, 3);
}
