//! Maps to: CC `hooks/useMemoryUsage.ts`.
//!
//! Node exposes `process.memoryUsage().heapUsed` (V8 heap). Cometix has no V8
//! heap; D6 samples **process RSS** into the same `heap_used` field / thresholds
//! so the ant-only footer indicator keeps official copy and gates.

/// Maps to: CC `hooks/useMemoryUsage.ts#MemoryUsageStatus`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MemoryUsageStatus {
    #[default]
    Normal,
    High,
    Critical,
}

/// Maps to: CC `hooks/useMemoryUsage.ts#MemoryUsageInfo`.
///
/// `heap_used` keeps the official field name; the value is process RSS bytes
/// (Rust stand-in for Node `heapUsed`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemoryUsageInfo {
    pub heap_used: u64,
    pub status: MemoryUsageStatus,
}

/// Maps to: CC `HIGH_MEMORY_THRESHOLD` (1.5 GiB).
pub const HIGH_MEMORY_THRESHOLD_BYTES: u64 = 3 * 1024 * 1024 * 1024 / 2;
/// Maps to: CC `CRITICAL_MEMORY_THRESHOLD` (2.5 GiB).
pub const CRITICAL_MEMORY_THRESHOLD_BYTES: u64 = 5 * 1024 * 1024 * 1024 / 2;

/// Maps to: CC `useMemoryUsage` status calculation from `heapUsed`.
pub fn memory_usage_status_for_bytes(bytes: u64) -> MemoryUsageStatus {
    if bytes >= CRITICAL_MEMORY_THRESHOLD_BYTES {
        MemoryUsageStatus::Critical
    } else if bytes >= HIGH_MEMORY_THRESHOLD_BYTES {
        MemoryUsageStatus::High
    } else {
        MemoryUsageStatus::Normal
    }
}

/// Deprecated alias — prefer [`memory_usage_status_for_bytes`].
pub fn memory_usage_status_for_heap(heap_used: u64) -> MemoryUsageStatus {
    memory_usage_status_for_bytes(heap_used)
}

/// Sample current process memory for the indicator (RSS → `heap_used`).
pub fn sample_memory_usage() -> MemoryUsageInfo {
    let heap_used = process_rss_bytes();
    MemoryUsageInfo {
        heap_used,
        status: memory_usage_status_for_bytes(heap_used),
    }
}

/// Process resident set size in bytes.
///
/// Maps to the *role* of CC `process.memoryUsage().heapUsed` for thresholding,
/// not the V8 heap counter itself.
pub fn process_rss_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            if let Some(kib) = parse_linux_vm_rss_kib(&status) {
                return kib.saturating_mul(1024);
            }
        }
        if let Ok(statm) = std::fs::read_to_string("/proc/self/statm") {
            if let Some(pages) = parse_linux_statm_rss_pages(&statm) {
                return pages.saturating_mul(linux_page_size_bytes());
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(bytes) = macos_task_resident_size_bytes() {
            return bytes;
        }
        let mut command = std::process::Command::new("ps");
        // Rust-only fallback; every child takes the carrier (process.env) as its base.
        crate::utils::subprocess_env::apply_process_env_std(&mut command);
        if let Ok(output) = command
            .args(["-o", "rss=", "-p"])
            .arg(std::process::id().to_string())
            .output()
        {
            if let Ok(text) = String::from_utf8(output.stdout) {
                if let Ok(kb) = text.trim().parse::<u64>() {
                    return kb.saturating_mul(1024);
                }
            }
        }
    }
    0
}

/// Alias kept for older call sites.
pub fn approximate_process_memory_bytes() -> u64 {
    process_rss_bytes()
}

/// Parse `VmRSS:` from `/proc/self/status` (value in KiB).
pub fn parse_linux_vm_rss_kib(status: &str) -> Option<u64> {
    for line in status.lines() {
        let Some(rest) = line.strip_prefix("VmRSS:") else {
            continue;
        };
        return rest.split_whitespace().next()?.parse().ok();
    }
    None
}

/// Parse resident pages from `/proc/self/statm` (2nd field).
pub fn parse_linux_statm_rss_pages(statm: &str) -> Option<u64> {
    statm.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(target_os = "linux")]
fn linux_page_size_bytes() -> u64 {
    // Prefer sysconf when available; 4096 covers common hosts as fallback.
    let page = unsafe { libc_sysconf_pagesize() };
    if page > 0 { page as u64 } else { 4096 }
}

#[cfg(target_os = "linux")]
unsafe fn libc_sysconf_pagesize() -> i64 {
    // `_SC_PAGESIZE` is 30 on Linux.
    unsafe extern "C" {
        fn sysconf(name: i32) -> i64;
    }
    unsafe { sysconf(30) }
}

#[cfg(target_os = "macos")]
fn macos_task_resident_size_bytes() -> Option<u64> {
    // MACH_TASK_BASIC_INFO — avoid depending on the `libc` crate.
    const MACH_TASK_BASIC_INFO: u32 = 20;
    const MACH_TASK_BASIC_INFO_COUNT: u32 =
        (std::mem::size_of::<MachTaskBasicInfo>() / std::mem::size_of::<u32>()) as u32;

    #[repr(C)]
    struct MachTaskBasicInfo {
        virtual_size: u64,
        resident_size: u64,
        resident_size_max: u64,
        user_time: TimeValue,
        system_time: TimeValue,
        policy: i32,
        suspend_count: i32,
    }

    #[repr(C)]
    struct TimeValue {
        seconds: i32,
        microseconds: i32,
    }

    unsafe extern "C" {
        fn mach_task_self() -> u32;
        fn task_info(
            target_task: u32,
            flavor: u32,
            task_info_out: *mut MachTaskBasicInfo,
            task_info_count: *mut u32,
        ) -> i32;
    }

    unsafe {
        let mut info = std::mem::MaybeUninit::<MachTaskBasicInfo>::uninit();
        let mut count = MACH_TASK_BASIC_INFO_COUNT;
        let kr = task_info(
            mach_task_self(),
            MACH_TASK_BASIC_INFO,
            info.as_mut_ptr(),
            &mut count,
        );
        if kr != 0 {
            return None;
        }
        Some(info.assume_init().resident_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_usage_status_matches_official_thresholds() {
        assert_eq!(
            memory_usage_status_for_bytes(HIGH_MEMORY_THRESHOLD_BYTES - 1),
            MemoryUsageStatus::Normal
        );
        assert_eq!(
            memory_usage_status_for_bytes(HIGH_MEMORY_THRESHOLD_BYTES),
            MemoryUsageStatus::High
        );
        assert_eq!(
            memory_usage_status_for_bytes(CRITICAL_MEMORY_THRESHOLD_BYTES),
            MemoryUsageStatus::Critical
        );
    }

    #[test]
    fn parse_linux_vm_rss_kib_reads_status_line() {
        let status = "Name:\tcometix\nVmSize:\t  123456 kB\nVmRSS:\t   2048 kB\n";
        assert_eq!(parse_linux_vm_rss_kib(status), Some(2048));
        assert_eq!(parse_linux_vm_rss_kib("VmSize:\t1 kB\n"), None);
    }

    #[test]
    fn parse_linux_statm_uses_resident_pages_not_virtual_size() {
        // size resident shared text lib data dt
        assert_eq!(
            parse_linux_statm_rss_pages("10000 1234 10 1 0 50 0"),
            Some(1234)
        );
    }

    #[test]
    fn process_rss_bytes_returns_nonzero_on_supported_hosts() {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let rss = process_rss_bytes();
            assert!(rss > 0, "expected live RSS sample, got {rss}");
        }
    }
}
