use std::sync::atomic::{AtomicBool, Ordering};

use log::error;
use windows::Win32::{
    Foundation::HINSTANCE,
    System::{
        Diagnostics::Debug::{SetUnhandledExceptionFilter, EXCEPTION_POINTERS},
        LibraryLoader::{GetModuleFileNameW, GetModuleHandleExW},
    },
};

static INSTALLED: AtomicBool = AtomicBool::new(false);

const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
const GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS: u32 = 0x00000004;
const GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT: u32 = 0x00000002;

/// Install a last-chance crash logger: logs rust panics and unhandled native
/// exceptions (e.g. access violations inside filament/assimp) to the tee
/// logger before the process dies. The exception filter returns
/// CONTINUE_SEARCH so WER still runs and can write a minidump (see
/// LocalDumps registry setup).
pub fn install() {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }

    std::panic::set_hook(Box::new(|panic_info| {
        error!(
            target: "CrashHandler",
            "rust panic (pid={}, tid={:?}): {}",
            std::process::id(),
            std::thread::current().id(),
            panic_info
        );
        log::logger().flush();
    }));

    unsafe {
        SetUnhandledExceptionFilter(Some(unhandled_exception_filter));
    }
}

unsafe extern "system" fn unhandled_exception_filter(info: *const EXCEPTION_POINTERS) -> i32 {
    let (code, address) = if !info.is_null() && !(*info).ExceptionRecord.is_null() {
        let record = &*(*info).ExceptionRecord;
        (record.ExceptionCode.0 as u32, record.ExceptionAddress as usize)
    } else {
        (0u32, 0usize)
    };

    let module_info = module_from_address(address)
        .map(|(name, base)| format!(" in {} (+0x{:X})", name, address - base))
        .unwrap_or_default();

    error!(
        target: "CrashHandler",
        "unhandled exception 0x{:08X} at 0x{:016X}{} (pid={}, tid={:?})",
        code,
        address,
        module_info,
        std::process::id(),
        std::thread::current().id()
    );
    log::logger().flush();

    // let the default handling (WER / LocalDumps minidump) proceed
    EXCEPTION_CONTINUE_SEARCH
}

unsafe fn module_from_address(address: usize) -> Option<(String, usize)> {
    if address == 0 {
        return None;
    }

    let mut module = HINSTANCE::default();
    let ok = GetModuleHandleExW(
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        windows::core::PCWSTR(address as *const u16),
        &mut module,
    );
    if !ok.as_bool() {
        return None;
    }

    let mut path = [0u16; 1024];
    let len = GetModuleFileNameW(module, path.as_mut_slice()) as usize;
    if len == 0 || len >= path.len() {
        return None;
    }

    Some((String::from_utf16_lossy(&path[..len]), module.0 as usize))
}
