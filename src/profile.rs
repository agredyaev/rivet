//! Lightweight performance instrumentation enabled by RIVET_PROFILE_FILE.

use serde_json::Value;
use std::{
    env,
    fs::{File, OpenOptions},
    io::Write,
    sync::{LazyLock, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

static PROFILE_FILE: LazyLock<Option<Mutex<File>>> = LazyLock::new(|| {
    let path = env::var_os("RIVET_PROFILE_FILE")?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
        .map(Mutex::new)
});

pub fn enabled() -> bool {
    PROFILE_FILE.is_some()
}

pub fn wall_ms() -> u128 {
    if !enabled() {
        return 0;
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

pub fn event(make_value: impl FnOnce() -> Value) {
    let Some(file) = PROFILE_FILE.as_ref() else {
        return;
    };
    let value = make_value();
    let Ok(mut bytes) = serde_json::to_vec(&value) else {
        return;
    };
    bytes.push(b'\n');
    let Ok(mut file) = file.lock() else {
        return;
    };
    let _ = file.write_all(&bytes);
}

fn rusage_peak_bytes(who: libc::c_int) -> u64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage initializes one rusage value for a valid selector.
    if unsafe { libc::getrusage(who, usage.as_mut_ptr()) } != 0 {
        return 0;
    }
    // SAFETY: getrusage succeeded and initialized the value.
    let usage = unsafe { usage.assume_init() };
    #[cfg(target_os = "macos")]
    {
        usage.ru_maxrss.max(0) as u64
    }
    #[cfg(target_os = "linux")]
    {
        (usage.ru_maxrss.max(0) as u64).saturating_mul(1024)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = usage;
        0
    }
}

pub fn self_peak_rss_bytes() -> u64 {
    if !enabled() {
        return 0;
    }
    rusage_peak_bytes(libc::RUSAGE_SELF)
}

pub fn children_peak_rss_bytes() -> u64 {
    if !enabled() {
        return 0;
    }
    rusage_peak_bytes(libc::RUSAGE_CHILDREN)
}

pub fn self_current_rss_bytes() -> u64 {
    if !enabled() {
        return 0;
    }
    #[cfg(target_os = "macos")]
    {
        macos_current_rss_bytes()
    }
    #[cfg(target_os = "linux")]
    {
        linux_current_rss_bytes()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        0
    }
}

#[cfg(target_os = "macos")]
#[allow(deprecated)]
fn macos_current_rss_bytes() -> u64 {
    let mut info = std::mem::MaybeUninit::<libc::mach_task_basic_info_data_t>::uninit();
    let mut count = (std::mem::size_of::<libc::mach_task_basic_info_data_t>()
        / std::mem::size_of::<libc::natural_t>())
        as libc::mach_msg_type_number_t;
    // SAFETY: task_info writes one MACH_TASK_BASIC_INFO value into the output buffer.
    let result = unsafe {
        libc::task_info(
            libc::mach_task_self_,
            libc::MACH_TASK_BASIC_INFO,
            info.as_mut_ptr().cast::<libc::integer_t>(),
            &mut count,
        )
    };
    if result != libc::KERN_SUCCESS {
        return 0;
    }
    // SAFETY: task_info returned KERN_SUCCESS and initialized the output buffer.
    unsafe { info.assume_init().resident_size }
}

#[cfg(target_os = "linux")]
fn linux_current_rss_bytes() -> u64 {
    let Ok(statm) = std::fs::read_to_string("/proc/self/statm") else {
        return 0;
    };
    let Some(pages) = statm
        .split_ascii_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u64>().ok())
    else {
        return 0;
    };
    // SAFETY: sysconf with _SC_PAGESIZE has no pointer arguments.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page_size <= 0 {
        return 0;
    }
    pages.saturating_mul(page_size as u64)
}
