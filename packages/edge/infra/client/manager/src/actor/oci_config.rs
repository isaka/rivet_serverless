use anyhow::*;
use serde_json::json;
use std::path::Path;

use super::{partial_oci_config::PartialOciConfigUser, seccomp};

pub struct ConfigOpts<'a> {
	pub actor_path: &'a Path,
	pub netns_path: &'a Path,
	pub args: Vec<String>,
	pub env: Vec<String>,
	pub user: PartialOciConfigUser,
	pub cwd: String,
	pub use_resource_constraints: bool,
	pub cpu: u64,
	pub memory: u64,
	pub memory_max: u64,
}

/// Generates base config.json for an OCI bundle.
///
/// Sanitize the config.json by copying safe properties from the provided bundle in to our base config.
pub fn config(opts: ConfigOpts) -> Result<serde_json::Value> {
	// CPU shares is a relative weight. It doesn't matter what unit we pass here as
	// long as the ratios between the actors are correct.
	//
	// Corresponds to cpu.weight in cgroups. Must be [1, 10_000]
	//
	// We divide by 10 in order to make sure the CPU shares are within bounds.
	let mut cpu_shares = opts.cpu / 10;
	if cpu_shares > 10_000 {
		cpu_shares = 10_000;
		tracing::warn!(?cpu_shares, "cpu_shares > 10_000");
	} else if cpu_shares < 1 {
		cpu_shares = 1;
		tracing::warn!(?cpu_shares, "cpu_shares < 1");
	}

	// This is a modified version of the default config.json generated by actord.
	//
	// Some values will be overridden at runtime by the values in the OCI bundle's config.json.
	//
	// Default Docker spec: https://github.com/moby/moby/blob/777e9f271095685543f30df0ff7a12397676f938/oci/defaults.go#L49
	//
	// Generate config.json with actord:
	// ctr run --rm -t --seccomp docker.io/library/debian:latest debian-actor-id /bin/bash
	// cat /run/actord/io.actord.runtime.v2.task/default/debian-actor-id/config.json | jq
	Ok(json!({
		"ociVersion": "1.0.2-dev",
		"process": {
			"args": opts.args,
			"env": opts.env,
			"user": opts.user,
			"cwd": opts.cwd,

			"terminal": false,
			"capabilities": {
				"bounding": capabilities(),
				"effective": capabilities(),
				"permitted": capabilities()
			},
			"rlimits": [
				{
					"type": "RLIMIT_NOFILE",
					"hard": 1024,
					"soft": 1024
				}
			],
			"noNewPrivileges": true

			// TODO: oomScoreAdj
			// TODO: scheduler
			// TODO: iopriority
			// TODO: rlimit?
		},
		"root": {
			"path": "rootfs",
			// This means we can't reuse the oci-bundle since the rootfs is writable.
			"readonly": false
		},
		"mounts": mounts(&opts)?,
		"linux": {
			"resources": {
				"devices": linux_resources_devices(),
				"cpu": if opts.use_resource_constraints {
					Some(json!({
						"shares": cpu_shares,
						// If `quota` is greater than `period`, it is allowed to use multiple cores.
						//
						// Read more: https://access.redhat.com/documentation/en-us/red_hat_enterprise_linux/6/html/resource_management_guide/sec-cpu
						// "quota": CPU_PERIOD * cpu / 1_000,
						// "period": CPU_PERIOD,
						// Use the env var for the CPU since Nomad handles assigning CPUs to each task
						// "cpus": if cpu >= 1_000 {
						// 	Some(template_env_var("NOMAD_CPU_CORES"))
						// } else {
						// 	None
						// }
					}))
				} else {
					None
				},
				// Docker: https://github.com/moby/moby/blob/777e9f271095685543f30df0ff7a12397676f938/daemon/daemon_unix.go#L75
				"memory": if opts.use_resource_constraints {
					Some(json!({
						"reservation": opts.memory,
						"limit": opts.memory_max,
					}))
				} else {
					None
				},

				// TODO: network
				// TODO: pids
				// TODO: hugepageLimits
				// TODO: blockIO
			},
			"namespaces": [
				{ "type": "pid" },
				{ "type": "ipc" },
				{ "type": "uts" },
				{ "type": "mount" },
				{ "type": "network", "path": opts.netns_path.to_str().context("netns_path")? },
			],
			"maskedPaths": [
				"/proc/acpi",
				"/proc/asound",
				"/proc/kcore",
				"/proc/keys",
				"/proc/latency_stats",
				"/proc/timer_list",
				"/proc/timer_stats",
				"/proc/sched_debug",
				"/sys/firmware",
				"/proc/scsi"
			],
			"readonlyPaths": [
				"/proc/bus",
				"/proc/fs",
				"/proc/irq",
				"/proc/sys",
				"/proc/sysrq-trigger"
			],
			"seccomp": seccomp::config()
		}
	}))
}

// Default Docker capabilities: https://github.com/moby/moby/blob/777e9f271095685543f30df0ff7a12397676f938/oci/caps/defaults.go#L4
fn capabilities() -> Vec<&'static str> {
	vec![
		"CAP_CHOWN",
		"CAP_DAC_OVERRIDE",
		"CAP_FSETID",
		"CAP_FOWNER",
		"CAP_MKNOD",
		"CAP_NET_RAW",
		"CAP_SETGID",
		"CAP_SETUID",
		"CAP_SETFCAP",
		"CAP_SETPCAP",
		"CAP_NET_BIND_SERVICE",
		"CAP_SYS_CHROOT",
		"CAP_KILL",
		"CAP_AUDIT_WRITE",
	]
}

fn mounts(opts: &ConfigOpts) -> Result<serde_json::Value> {
	Ok(json!([
		{
			"destination": "/proc",
			"type": "proc",
			"source": "proc",
			"options": [
				"nosuid",
				"noexec",
				"nodev"
			]
		},
		{
			"destination": "/dev",
			"type": "tmpfs",
			"source": "tmpfs",
			"options": [
				"nosuid",
				"strictatime",
				"mode=755",
				"size=65536k"
			]
		},
		{
			"destination": "/dev/pts",
			"type": "devpts",
			"source": "devpts",
			"options": [
				"nosuid",
				"noexec",
				"newinstance",
				"ptmxmode=0666",
				"mode=0620",
				"gid=5"
			]
		},
		{
			"destination": "/dev/shm",
			"type": "tmpfs",
			"source": "shm",
			"options": [
				"nosuid",
				"noexec",
				"nodev",
				"mode=1777",
				"size=65536k"
			]
		},
		{
			"destination": "/dev/mqueue",
			"type": "mqueue",
			"source": "mqueue",
			"options": [
				"nosuid",
				"noexec",
				"nodev"
			]
		},
		{
			"destination": "/sys",
			"type": "sysfs",
			"source": "sysfs",
			"options": [
				"nosuid",
				"noexec",
				"nodev",
				"ro"
		]
		},
		{
			"destination": "/run",
			"type": "tmpfs",
			"source": "tmpfs",
			"options": [
				"nosuid",
				"strictatime",
				"mode=755",
				"size=65536k"
			]
		},
		{
			"destination": "/etc/resolv.conf",
			"type": "bind",
			"source": opts.actor_path.join("resolv.conf").to_str().context("resolv.conf path")?,
			"options": ["rbind", "rprivate"]
		},
		{
			"destination": "/etc/hosts",
			"type": "bind",
			"source": opts.actor_path.join("hosts").to_str().context("hosts path")?,
			"options": ["rbind", "rprivate"]
		},
	]))
}

fn linux_resources_devices() -> serde_json::Value {
	// Devices implicitly contains the following devices:
	// null, zero, full, random, urandom, tty, console, and ptmx.
	// ptmx is a bind mount or symlink of the actor's ptmx.
	// See also: https://github.com/openactors/runtime-spec/blob/master/config-linux.md#default-devices
	json!([
		{
		"allow": false,
		"access": "rwm"
		},
		{
		"allow": true,
		"type": "c",
		"major": 1,
		"minor": 3,
		"access": "rwm"
		},
		{
		"allow": true,
		"type": "c",
		"major": 1,
		"minor": 8,
		"access": "rwm"
		},
		{
		"allow": true,
		"type": "c",
		"major": 1,
		"minor": 7,
		"access": "rwm"
		},
		{
		"allow": true,
		"type": "c",
		"major": 5,
		"minor": 0,
		"access": "rwm"
		},
		{
		"allow": true,
		"type": "c",
		"major": 1,
		"minor": 5,
		"access": "rwm"
		},
		{
		"allow": true,
		"type": "c",
		"major": 1,
		"minor": 9,
		"access": "rwm"
		},
		{
		"allow": true,
		"type": "c",
		"major": 5,
		"minor": 1,
		"access": "rwm"
		},
		{
		"allow": true,
		"type": "c",
		"major": 136,
		"access": "rwm"
		},
		{
		"allow": true,
		"type": "c",
		"major": 5,
		"minor": 2,
		"access": "rwm"
		}
	])
}
