#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::Path;

use wyrd_doctor::{
    CellPath, CheckId, Env, PrivilegedHelper, Probes, Status, parse_kernel_version, render_text,
    run,
};

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
            ..Probes::default()
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
    assert_eq!(r.cell_path, None);
    assert_eq!(r.privileged_helper, PrivilegedHelper::NotConfigured);
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
    assert_eq!(r.cell_path, Some(CellPath::UserNamespace));
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

#[test]
fn sensord_check_probes_the_configured_socket() {
    // `WYRD_SENSORD_SOCKET` moves the socket the client connects to. The doctor
    // must probe that path, not the system default under the root.
    let dir = tempfile::tempdir().unwrap();
    ubuntu_2404(dir.path());
    write(dir.path(), "run/wyrdline/sensord.sock", "");
    let scratch = tempfile::tempdir().unwrap();
    let configured = scratch.path().join("sensord.sock");

    let r = run(&env(dir.path(), Some(7)).with_sensord_socket(&configured));
    let c = r.get(CheckId::Sensord).unwrap();
    assert_eq!(c.status, Status::Missing, "the root default must not count");
    assert!(
        c.detail.contains(&configured.display().to_string()),
        "the detail names the probed path: {}",
        c.detail
    );

    write(scratch.path(), "sensord.sock", "");
    let r = run(&env(dir.path(), Some(7)).with_sensord_socket(&configured));
    assert_eq!(r.get(CheckId::Sensord).unwrap().status, Status::Ok);
}

#[test]
fn verified_helper_makes_cell_mode_available() {
    // The sudo helper stands in for the user namespace. The userns check itself
    // is unchanged: it stays degraded and the report says which path works.
    let dir = tempfile::tempdir().unwrap();
    ubuntu_2404(dir.path());
    let mut e = env(dir.path(), Some(7));
    e.probes.privileged_helper = PrivilegedHelper::Verified;
    let r = run(&e);
    assert!(r.cell_mode);
    assert_eq!(r.cell_path, Some(CellPath::PrivilegedHelper));
    assert_eq!(r.privileged_helper, PrivilegedHelper::Verified);
    let userns = r.get(CheckId::Userns).unwrap();
    assert_eq!(userns.status, Status::Degraded);
    assert!(userns.detail.contains("AppArmor"));
    let text = render_text(&r);
    assert!(text.contains("Cell mode: yes"));
    assert!(text.contains("privileged helper"));
}

#[test]
fn refused_helper_keeps_cell_mode_off_and_says_why() {
    let dir = tempfile::tempdir().unwrap();
    ubuntu_2404(dir.path());
    let mut e = env(dir.path(), Some(7));
    e.probes.privileged_helper = PrivilegedHelper::Refused("sudo: a password is required".into());
    let r = run(&e);
    assert!(!r.cell_mode);
    assert_eq!(r.cell_path, None);
    assert_eq!(
        r.privileged_helper,
        PrivilegedHelper::Refused("sudo: a password is required".into())
    );
    assert!(render_text(&r).contains("a password is required"));
}

#[test]
fn unprobed_helper_is_not_counted() {
    // The daemon reports without running sudo; it must not claim Cell mode.
    let dir = tempfile::tempdir().unwrap();
    ubuntu_2404(dir.path());
    let mut e = env(dir.path(), Some(7));
    e.probes.privileged_helper = PrivilegedHelper::NotProbed;
    let r = run(&e);
    assert!(!r.cell_mode);
    assert_eq!(r.privileged_helper, PrivilegedHelper::NotProbed);
}

#[test]
fn verified_helper_does_not_skip_the_required_checks() {
    let dir = tempfile::tempdir().unwrap();
    ubuntu_2404(dir.path());
    let mut e = env(dir.path(), None);
    e.probes.privileged_helper = PrivilegedHelper::Verified;
    let r = run(&e);
    assert_eq!(r.get(CheckId::Landlock).unwrap().status, Status::Unknown);
    assert!(!r.cell_mode);
}

#[test]
fn probed_helper_runs_the_probe_only_when_configured() {
    let mut calls = 0;
    let off = PrivilegedHelper::probed(false, || {
        calls += 1;
        Ok(())
    });
    assert_eq!(off, PrivilegedHelper::NotConfigured);
    assert_eq!(calls, 0, "no sudo run unless WYRD_PRIVILEGED_HELPER=sudo");

    let on = PrivilegedHelper::probed(true, || Err("no sudoers rule".into()));
    assert_eq!(on, PrivilegedHelper::Refused("no sudoers rule".into()));
    assert_eq!(
        PrivilegedHelper::probed(true, || Ok(())),
        PrivilegedHelper::Verified
    );
    assert_eq!(
        PrivilegedHelper::unprobed(true),
        PrivilegedHelper::NotProbed
    );
    assert_eq!(
        PrivilegedHelper::unprobed(false),
        PrivilegedHelper::NotConfigured
    );
}
