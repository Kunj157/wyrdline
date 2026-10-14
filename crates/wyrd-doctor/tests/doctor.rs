#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::Path;

use wyrd_doctor::{CheckId, Env, Probes, Status, parse_kernel_version, render_text, run};

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

/// The development machine: Ubuntu 24.04, kernel 6.17, Landlock, no BPF-LSM,
/// AppArmor-restricted user namespaces.
fn ubuntu_2404(root: &Path) {
    write(root, "proc/sys/kernel/osrelease", "6.17.0-14-generic\n");
    write(root, "sys/kernel/btf/vmlinux", "");
    write(
        root,
        "sys/fs/cgroup/cgroup.controllers",
        "cpu io memory pids\n",
    );
    write(
        root,
        "sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/cgroup.procs",
        "",
    );
    write(
        root,
        "sys/kernel/security/lsm",
        "lockdown,capability,landlock,yama,apparmor,ima,evm",
    );
    write(root, "proc/sys/user/max_user_namespaces", "253396\n");
    write(root, "proc/sys/kernel/unprivileged_userns_clone", "1\n");
    write(
        root,
        "proc/sys/kernel/apparmor_restrict_unprivileged_userns",
        "1\n",
    );
    write(root, "proc/filesystems", "nodev\tsysfs\nnodev\toverlay\n");
}

fn env(root: &Path, abi: Option<i32>) -> Env {
    Env::new(
        root,
        Probes {
            landlock_abi: abi,
            uid: 1000,
        },
    )
}

#[test]
fn ubuntu_2404_profile() {
    let dir = tempfile::tempdir().unwrap();
    ubuntu_2404(dir.path());
    let r = run(&env(dir.path(), Some(7)));
    let status = |id| r.get(id).unwrap().status;
    assert_eq!(r.kernel_release, "6.17.0-14-generic");
    assert_eq!(status(CheckId::Kernel), Status::Ok);
    assert_eq!(status(CheckId::Btf), Status::Ok);
    assert_eq!(status(CheckId::Ringbuf), Status::Ok);
    assert_eq!(status(CheckId::CgroupV2), Status::Ok);
    assert_eq!(status(CheckId::CgroupDelegation), Status::Ok);
    assert_eq!(status(CheckId::Landlock), Status::Ok);
    assert_eq!(status(CheckId::BpfLsm), Status::Degraded);
    assert_eq!(status(CheckId::Userns), Status::Degraded);
    assert_eq!(status(CheckId::OverlayUserns), Status::Ok);
    assert_eq!(status(CheckId::CgroupSockAddr), Status::Ok);
    assert_eq!(status(CheckId::Sensord), Status::Missing);
    // No AppArmor profile and no sensor: user namespaces are unusable.
    assert!(!r.cell_mode);
    assert!(r.kernel_capture);
    assert!(r.kernel_network_enforcement);
    assert!(r.get(CheckId::Landlock).unwrap().detail.contains("ABI v7"));
}

#[test]
fn bpf_lsm_kernel_and_sensord() {
    let dir = tempfile::tempdir().unwrap();
    ubuntu_2404(dir.path());
    write(
        dir.path(),
        "sys/kernel/security/lsm",
        "lockdown,capability,landlock,bpf",
    );
    write(dir.path(), "run/wyrdline/sensord.sock", "");
    let r = run(&env(dir.path(), Some(7)));
    assert_eq!(r.get(CheckId::BpfLsm).unwrap().status, Status::Ok);
    assert_eq!(r.get(CheckId::Sensord).unwrap().status, Status::Ok);
    assert!(r.cell_mode, "sensord can create the namespaces");
}

#[test]
fn apparmor_profile_allows_userns() {
    let dir = tempfile::tempdir().unwrap();
    ubuntu_2404(dir.path());
    write(dir.path(), "etc/apparmor.d/wyrd", "profile wyrd {}");
    let r = run(&env(dir.path(), Some(7)));
    assert_eq!(r.get(CheckId::Userns).unwrap().status, Status::Ok);
    assert!(r.cell_mode);
}

#[test]
fn old_kernel_without_landlock_or_btf() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "proc/sys/kernel/osrelease", "5.4.0-150-generic");
    write(dir.path(), "sys/fs/cgroup/cgroup.controllers", "");
    write(dir.path(), "sys/kernel/security/lsm", "capability,apparmor");
    write(dir.path(), "proc/sys/user/max_user_namespaces", "1000");
    write(dir.path(), "proc/filesystems", "nodev\toverlay\n");
    let r = run(&env(dir.path(), None));
    assert_eq!(r.get(CheckId::Kernel).unwrap().status, Status::Degraded);
    assert_eq!(r.get(CheckId::Btf).unwrap().status, Status::Missing);
    assert_eq!(r.get(CheckId::Ringbuf).unwrap().status, Status::Missing);
    assert_eq!(r.get(CheckId::Landlock).unwrap().status, Status::Missing);
    assert_eq!(
        r.get(CheckId::OverlayUserns).unwrap().status,
        Status::Missing
    );
    assert!(!r.cell_mode);
    assert!(!r.kernel_capture);
}

#[test]
fn cgroup_v1_host() {
    let dir = tempfile::tempdir().unwrap();
    ubuntu_2404(dir.path());
    fs::remove_file(dir.path().join("sys/fs/cgroup/cgroup.controllers")).unwrap();
    let r = run(&env(dir.path(), Some(7)));
    assert_eq!(r.get(CheckId::CgroupV2).unwrap().status, Status::Missing);
    assert_eq!(
        r.get(CheckId::CgroupSockAddr).unwrap().status,
        Status::Missing
    );
    assert_eq!(
        r.get(CheckId::CgroupDelegation).unwrap().status,
        Status::Degraded
    );
    assert!(!r.cell_mode);
    assert!(!r.kernel_network_enforcement);
}

#[test]
fn old_landlock_abi_is_degraded() {
    let dir = tempfile::tempdir().unwrap();
    ubuntu_2404(dir.path());
    write(dir.path(), "etc/apparmor.d/wyrd", "");
    let r = run(&env(dir.path(), Some(2)));
    assert_eq!(r.get(CheckId::Landlock).unwrap().status, Status::Degraded);
    assert!(!r.cell_mode);
}

#[test]
fn userns_disabled() {
    let dir = tempfile::tempdir().unwrap();
    ubuntu_2404(dir.path());
    write(dir.path(), "proc/sys/kernel/unprivileged_userns_clone", "0");
    let r = run(&env(dir.path(), Some(7)));
    assert_eq!(r.get(CheckId::Userns).unwrap().status, Status::Missing);
}

#[test]
fn empty_root_reports_unknown_without_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let r = run(&env(dir.path(), None));
    assert_eq!(r.get(CheckId::Kernel).unwrap().status, Status::Unknown);
    assert_eq!(r.get(CheckId::Userns).unwrap().status, Status::Unknown);
    assert!(!r.cell_mode);
    let _ = render_text(&r);
}

#[test]
fn mask_forces_missing() {
    let dir = tempfile::tempdir().unwrap();
    ubuntu_2404(dir.path());
    let mut e = env(dir.path(), Some(7));
    e.mask = Some("landlock, btf,nonsense".into());
    let r = run(&e);
    assert_eq!(r.get(CheckId::Landlock).unwrap().status, Status::Missing);
    assert_eq!(r.get(CheckId::Btf).unwrap().status, Status::Missing);
    assert_eq!(r.masked, vec!["landlock", "btf"]);
    assert!(!r.cell_mode);
    assert!(!r.kernel_capture);
    assert!(render_text(&r).contains("Masked: landlock,btf"));
}

#[test]
fn json_shape_and_capabilities() {
    let dir = tempfile::tempdir().unwrap();
    ubuntu_2404(dir.path());
    let r = run(&env(dir.path(), Some(7)));
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["checks"].as_array().unwrap().len(), CheckId::ALL.len());
    assert_eq!(v["checks"][0]["id"], "kernel");
    assert_eq!(v["checks"][0]["status"], "ok");
    let caps = r.capabilities();
    assert_eq!(caps.get("landlock"), Some(&true));
    assert_eq!(caps.get("bpf_lsm"), Some(&false));
}

#[test]
fn kernel_version_parsing() {
    assert_eq!(parse_kernel_version("6.17.0-14-generic"), Some((6, 17, 0)));
    assert_eq!(parse_kernel_version("5.15"), Some((5, 15, 0)));
    assert_eq!(parse_kernel_version("6.1.112+deb12"), Some((6, 1, 112)));
    assert_eq!(parse_kernel_version("garbage"), None);
    assert_eq!(parse_kernel_version(""), None);
}
