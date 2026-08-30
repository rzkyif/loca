//! Hardware detection for the fit engine.
//!
//! Three things are deliberate. The Vulkan probe is gated to Linux and Windows
//! rather than to "not macOS", because `loca-core-ffi` is a staticlib and a
//! "not macOS" gate would archive a Vulkan loader into the mobile build for a
//! probe it never runs. Detection is cached on the `Models` handle rather than
//! in a module global, so a test builds its own and there is nothing to reset.
//! And the macOS CPU brand comes from `sysinfo` rather than a `sysctl`
//! subprocess, which reads the same `machdep.cpu.brand_string`.
//!
//! Detection never fails. Every probe returns an `Option` and the chain
//! terminates at a CPU backend with no VRAM, because a wrong-but-conservative
//! answer still lets the fit engine recommend something.

use sysinfo::System;

use crate::contract::api::{GpuBackend, GpuInfo, HardwareInfo};

/// Probe the machine. Blocking: it reads sysctls and, on Linux and Windows,
/// spawns the vendor CLI tools. Callers on an async path go through
/// `Models::hardware`, which moves it off the runtime.
pub fn detect() -> HardwareInfo {
    let mut sys = System::new();
    sys.refresh_memory();
    sys.refresh_cpu_all();
    let total_ram_bytes = sys.total_memory();
    let available_ram_bytes = sys.available_memory();
    let (gpu, unified_memory) = detect_gpu(&sys, total_ram_bytes);

    HardwareInfo {
        total_ram_bytes: total_ram_bytes as i64,
        available_ram_bytes: available_ram_bytes as i64,
        cpu_cores_physical: physical_cores(),
        gpu,
        unified_memory,
    }
}

/// Physical cores, with two fallbacks. The final 4 matters: `threads_for` clamps
/// with `max(1, ...)`, so a zero here would silently halve inference throughput
/// rather than crash.
fn physical_cores() -> i64 {
    System::physical_core_count()
        .or_else(|| std::thread::available_parallelism().ok().map(|n| n.get()))
        .unwrap_or(4) as i64
}

/// The CPU brand, which doubles as the Apple GPU's name. Unused on the mobile
/// targets, which report a CPU backend and a fixed name.
#[cfg_attr(any(target_os = "android", target_os = "ios"), allow(dead_code))]
fn cpu_brand(sys: &System) -> Option<String> {
    let brand = sys.cpus().first()?.brand().trim().to_string();
    if brand.is_empty() {
        None
    } else {
        Some(brand)
    }
}

/// Mobile. **Both phone targets are CPU-only, and this is the branch that has to
/// say so**, because the fit engine turns `gpu.backend` straight into
/// `gpuLayers`: any non-CPU backend resolves to 999, "offload every layer", and
/// `build.rs` defines `GGML_METAL=ON` for macOS only.
///
/// `unified_memory: true` is not a claim about a GPU: it is what makes
/// `memory_budget_bytes` use total RAM rather than a VRAM figure, which is the
/// correct model for a phone.
///
/// **iOS is excluded from the Apple branches explicitly**, by the
/// `not(target_os = "ios")` on each of them. `#[cfg]` has no "first match wins",
/// so two enabled definitions of `detect_gpu` are a duplicate-definition compile
/// error rather than a fallthrough.
#[cfg(any(target_os = "android", target_os = "ios"))]
fn detect_gpu(_sys: &System, _total_ram_bytes: u64) -> (GpuInfo, bool) {
    mobile_gpu()
}

/// The mobile answer, compiled on EVERY platform so it can be tested: the
/// `cfg`-gated `detect_gpu` above cannot be run by a host test binary.
#[cfg_attr(not(any(target_os = "android", target_os = "ios")), allow(dead_code))]
fn mobile_gpu() -> (GpuInfo, bool) {
    (
        GpuInfo {
            backend: GpuBackend::Cpu,
            name: "CPU".into(),
            vram_bytes: 0,
        },
        // Unified: budget from total RAM. See the note above.
        true,
    )
}

/// The terminal answer when no probe succeeded. Only the non-Apple branch
/// reaches it: Metal is always present on an Apple target.
#[cfg(not(any(target_vendor = "apple", target_os = "android")))]
fn cpu_gpu() -> (GpuInfo, bool) {
    (
        GpuInfo {
            backend: GpuBackend::Cpu,
            name: "CPU".into(),
            vram_bytes: 0,
        },
        false,
    )
}

/// Apple Silicon on macOS, where the GPU shares system RAM and Metal is always
/// present AND linked. iOS is excluded and handled by the mobile branch above:
/// it is an Apple aarch64 target with no Metal backend in the binary.
#[cfg(all(
    target_vendor = "apple",
    target_arch = "aarch64",
    not(target_os = "ios")
))]
fn detect_gpu(sys: &System, total_ram_bytes: u64) -> (GpuInfo, bool) {
    (
        GpuInfo {
            backend: GpuBackend::Metal,
            name: cpu_brand(sys).unwrap_or_else(|| "Apple Silicon".into()),
            vram_bytes: total_ram_bytes as i64,
        },
        true,
    )
}

/// Intel Mac: Metal is available but memory is not unified, so there is no VRAM
/// figure to report and the budget falls back to system RAM.
#[cfg(all(
    target_vendor = "apple",
    not(target_arch = "aarch64"),
    not(target_os = "ios")
))]
fn detect_gpu(_sys: &System, _total_ram_bytes: u64) -> (GpuInfo, bool) {
    (
        GpuInfo {
            backend: GpuBackend::Metal,
            name: "Mac GPU".into(),
            vram_bytes: 0,
        },
        false,
    )
}

/// Everything else. The vendor probes come first because they carry accurate
/// VRAM and drive the cuda/rocm binary-variant choice; Vulkan catches whatever
/// has a GPU that neither vendor tool knows about, notably Intel.
#[cfg(not(any(target_vendor = "apple", target_os = "android")))]
fn detect_gpu(_sys: &System, _total_ram_bytes: u64) -> (GpuInfo, bool) {
    probe_nvidia()
        .or_else(probe_rocm)
        .or_else(probe_vulkan)
        .map(|gpu| (gpu, false))
        .unwrap_or_else(cpu_gpu)
}

/// Run a command and return its trimmed stdout, or `None` on any failure.
#[cfg(not(any(target_vendor = "apple", target_os = "android")))]
fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Parse `nvidia-smi --query-gpu=memory.total,name --format=csv,noheader,nounits`,
/// e.g. `24576, NVIDIA GeForce RTX 4090`, where the memory is in MiB.
///
/// Pure, so it is testable against recorded output; the I/O lives in
/// [`probe_nvidia`]. Compiled on every platform because only the tests use it
/// on Apple targets.
#[cfg_attr(any(target_vendor = "apple", target_os = "android"), allow(dead_code))]
fn parse_nvidia(out: &str) -> Option<GpuInfo> {
    let line = out.lines().next()?;
    let (mib, name) = line.split_once(',')?;
    let mib: u64 = mib.trim().parse().ok()?;
    Some(GpuInfo {
        backend: GpuBackend::Cuda,
        name: name.trim().to_string(),
        vram_bytes: (mib * 1024 * 1024) as i64,
    })
}

#[cfg(not(any(target_vendor = "apple", target_os = "android")))]
fn probe_nvidia() -> Option<GpuInfo> {
    let out = run(
        "nvidia-smi",
        &[
            "--query-gpu=memory.total,name",
            "--format=csv,noheader,nounits",
        ],
    )?;
    parse_nvidia(&out)
}

/// Parse total VRAM in bytes from `rocm-smi --showmeminfo vram --csv`. The CSV
/// has a header and then one row per GPU; the largest integer across the data
/// cells is the total-VRAM column.
#[cfg_attr(any(target_vendor = "apple", target_os = "android"), allow(dead_code))]
fn parse_rocm(out: &str) -> Option<GpuInfo> {
    let vram = out
        .lines()
        .skip(1)
        .flat_map(|l| l.split(','))
        .filter_map(|c| c.trim().parse::<u64>().ok())
        .max()?;
    if vram == 0 {
        return None;
    }
    Some(GpuInfo {
        backend: GpuBackend::Rocm,
        name: "AMD GPU".into(),
        vram_bytes: vram as i64,
    })
}

#[cfg(not(any(target_vendor = "apple", target_os = "android")))]
fn probe_rocm() -> Option<GpuInfo> {
    let out = run("rocm-smi", &["--showmeminfo", "vram", "--csv"])?;
    parse_rocm(&out)
}

/// Any Vulkan-capable discrete or integrated GPU the vendor probes missed.
/// Loads the system Vulkan loader at runtime, so no SDK is needed at build time
/// and a host with no driver simply returns `None`. Picks the first non-CPU
/// physical device and reports its largest device-local heap as VRAM.
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn probe_vulkan() -> Option<GpuInfo> {
    use ash::vk;
    use std::ffi::CStr;

    let entry = unsafe { ash::Entry::load() }.ok()?;
    let app_info = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_0);
    let create_info = vk::InstanceCreateInfo::default().application_info(&app_info);
    let instance = unsafe { entry.create_instance(&create_info, None) }.ok()?;

    let result = (|| {
        let devices = unsafe { instance.enumerate_physical_devices() }.ok()?;
        for pd in devices {
            let props = unsafe { instance.get_physical_device_properties(pd) };
            if props.device_type == vk::PhysicalDeviceType::CPU {
                continue;
            }
            let mem = unsafe { instance.get_physical_device_memory_properties(pd) };
            let vram = mem.memory_heaps[..mem.memory_heap_count as usize]
                .iter()
                .filter(|h| h.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL))
                .map(|h| h.size)
                .max()
                .unwrap_or(0);
            let name = unsafe { CStr::from_ptr(props.device_name.as_ptr()) }
                .to_string_lossy()
                .into_owned();
            return Some(GpuInfo {
                backend: GpuBackend::Vulkan,
                name,
                vram_bytes: vram as i64,
            });
        }
        None
    })();

    unsafe { instance.destroy_instance(None) };
    result
}

/// Every other non-Apple target: no Vulkan probe. Android is NOT here any more;
/// it takes the mobile branch above and never reaches any probe, which is the
/// point of that branch.
#[cfg(all(
    not(any(target_vendor = "apple", target_os = "android")),
    not(any(target_os = "linux", target_os = "windows"))
))]
fn probe_vulkan() -> Option<GpuInfo> {
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn parse_nvidia_reads_mib_and_name() {
        let g = parse_nvidia("24576, NVIDIA GeForce RTX 4090\n").expect("parsed");
        assert_eq!(g.backend, GpuBackend::Cuda);
        assert_eq!(g.name, "NVIDIA GeForce RTX 4090");
        assert_eq!(g.vram_bytes, 24576 * 1024 * 1024);
    }

    #[test]
    fn parse_nvidia_uses_only_the_first_gpu_row() {
        let g = parse_nvidia("8192, GPU A\n16384, GPU B\n").expect("parsed");
        assert_eq!(g.name, "GPU A");
        assert_eq!(g.vram_bytes, 8192 * 1024 * 1024);
    }

    #[test]
    fn parse_nvidia_rejects_malformed_output() {
        assert!(parse_nvidia("").is_none());
        assert!(parse_nvidia("no comma here").is_none());
        assert!(parse_nvidia("notanumber, GPU\n").is_none());
    }

    #[test]
    fn parse_rocm_picks_the_largest_byte_count() {
        let csv = "device,Total Memory (B),Used Memory (B)\ncard0,17163091968,1048576\n";
        let g = parse_rocm(csv).expect("parsed");
        assert_eq!(g.backend, GpuBackend::Rocm);
        assert_eq!(g.vram_bytes, 17163091968);
    }

    #[test]
    fn parse_rocm_rejects_zero_or_headerless_output() {
        assert!(parse_rocm("header\ncard0,0\n").is_none());
        assert!(parse_rocm("header only\n").is_none());
        assert!(parse_rocm("").is_none());
    }

    /// Whatever the detection path, the count has to be a positive number: the
    /// fit engine turns it into a thread count and zero would be a machine that
    /// silently runs inference on one core.
    #[test]
    fn detection_reports_a_usable_machine() {
        let hw = detect();
        assert!(hw.cpu_cores_physical >= 1);
        assert!(hw.total_ram_bytes > 0, "a machine has memory");
        // Unified memory means the GPU budget IS system RAM, which the fit
        // engine relies on when choosing the pool to fit into.
        // Narrowed after F-H: the mobile build reports unified memory with a
        // CPU backend and zero VRAM, so "unified implies the GPU budget IS
        // system RAM" is false there. It remains true for the Apple Silicon
        // case this branch is about, which is the only one that can run here.
        if hw.unified_memory && hw.gpu.backend != GpuBackend::Cpu {
            assert_eq!(hw.gpu.vram_bytes, hw.total_ram_bytes);
            assert_ne!(hw.gpu.backend, GpuBackend::Cpu);
        }
    }

    /// The failure mode the fit engine's `|| 4` exists for. Detection is
    /// allowed to be wrong; it is not allowed to be zero.
    #[test]
    fn a_second_probe_agrees_with_the_first_on_the_stable_facts() {
        let a = detect();
        let b = detect();
        assert_eq!(a.total_ram_bytes, b.total_ram_bytes);
        assert_eq!(a.cpu_cores_physical, b.cpu_cores_physical);
        assert_eq!(a.gpu.backend, b.gpu.backend);
        assert_eq!(a.gpu.name, b.gpu.name);
        // `available_ram_bytes` deliberately excluded: it is momentary, which
        // is exactly why the memory budget is computed from the total.
    }

    /// F-H. Q7's ruling was "fix it in mobile hardware detection, with a test",
    /// and the evidence originally offered was that it compiles.
    ///
    /// What this pins is the answer the mobile builds return, which is what the
    /// fit engine turns into `gpuLayers`: any non-CPU backend resolves to 999,
    /// "offload every layer", and iOS previously reported `Metal` while
    /// `build.rs` defines `GGML_METAL=ON` for macOS only. So an iPhone was being
    /// told to offload everything to a backend the binary does not contain.
    ///
    /// It runs on every host because `mobile_gpu` is compiled on every host.
    /// What it cannot check is that the two `cfg` arms select it, which only a
    /// cross-compile can, and `deno task mobile-check` covers that.
    #[test]
    fn the_mobile_answer_is_cpu_only_with_a_unified_budget() {
        let (gpu, unified) = mobile_gpu();
        assert_eq!(
            gpu.backend,
            GpuBackend::Cpu,
            "a non-CPU backend resolves to gpuLayers 999, and mobile links none",
        );
        assert_eq!(
            gpu.vram_bytes, 0,
            "there is no separate VRAM pool on a phone"
        );
        assert!(
            unified,
            "unified is what makes memory_budget_bytes use total RAM, which is \
             the model the mobile fit profile is built on",
        );
    }
}
