//! 注入式光标读取（可选，默认关闭）。
//!
//! 为什么需要它：Chromium/Electron/Qt 这类程序不通过公开 API 暴露文本光标，
//! 旧版 InputTip 的做法是往目标进程注入 shellcode 直接调 GetCaretPos。
//! 我们改成注入一个正规 DLL（hook/src/lib.rs）：只在里面读光标，不改任何状态。
//!
//! 默认关闭（配置 [inject] enabled=false），因为：
//! - 未签名的 DLL 注入容易被杀软误报；
//! - 需要老大明确同意后才对指定程序启用。

use std::collections::HashSet;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;

use windows::core::HSTRING;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, MapViewOfFile,
    PAGE_READWRITE, VirtualAllocEx, VirtualFreeEx,
};
use windows::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows::Win32::System::ProcessStatus::{
    K32EnumProcessModulesEx, K32GetModuleBaseNameW, LIST_MODULES_ALL,
};
use windows::Win32::System::Threading::{
    CreateRemoteThread, GetCurrentProcess, GetExitCodeThread, OpenProcess, PROCESS_CREATE_THREAD,
    PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
    WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowThreadProcessId, SMTO_ABORTIFHUNG, SendMessageTimeoutW,
};

/// 与 DLL 里保持一致
pub const HOOK_QUERY_MSG: u32 = 0x8000 + 0x51;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CaretShared {
    seq: u32,
    left: i32,
    top: i32,
    width: i32,
    height: i32,
}

thread_local! {
    static SHARED: std::cell::RefCell<Option<*mut CaretShared>> = const { std::cell::RefCell::new(None) };
    static ATTACHED: std::cell::RefCell<HashSet<u32>> = std::cell::RefCell::new(HashSet::new());
    static SEQ: std::cell::Cell<u32> = const { std::cell::Cell::new(1) };
}

fn shared() -> Option<*mut CaretShared> {
    SHARED.with(|slot| {
        if slot.borrow().is_none() {
            unsafe {
                let name = HSTRING::from("Local\\input-tip-ng-caret");
                let handle = CreateFileMappingW(
                    HANDLE::default(),
                    None,
                    PAGE_READWRITE,
                    0,
                    std::mem::size_of::<CaretShared>() as u32,
                    &name,
                )
                .ok()?;
                let view = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0);
                if view.Value.is_null() {
                    return None;
                }
                *slot.borrow_mut() = Some(view.Value as *mut CaretShared);
            }
        }
        *slot.borrow()
    })
}

fn dll_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let p = dir.join("input-tip-hook.dll");
    if p.exists() { Some(p) } else { None }
}

unsafe fn module_base(process: HANDLE, name: &str) -> Option<usize> {
    let mut modules = [windows::Win32::Foundation::HMODULE::default(); 512];
    let mut needed = 0u32;
    let ok = unsafe {
        K32EnumProcessModulesEx(
            process,
            modules.as_mut_ptr(),
            (modules.len() * std::mem::size_of::<isize>()) as u32,
            &mut needed,
            LIST_MODULES_ALL.0,
        )
    };
    if !ok.as_bool() {
        return None;
    }
    let count = (needed as usize / std::mem::size_of::<isize>()).min(modules.len());
    for module in &modules[..count] {
        let mut buf = [0u16; 260];
        let len = unsafe { K32GetModuleBaseNameW(process, Some(*module), &mut buf) };
        if len == 0 {
            continue;
        }
        let current = String::from_utf16_lossy(&buf[..len as usize]);
        if current.eq_ignore_ascii_case(name) {
            return Some(module.0 as usize);
        }
    }
    None
}

fn inject(pid: u32, hwnd: HWND) -> bool {
    let Some(dll) = dll_path() else { return false };
    unsafe {
        let access = PROCESS_CREATE_THREAD
            | PROCESS_VM_OPERATION
            | PROCESS_VM_WRITE
            | PROCESS_VM_READ
            | PROCESS_QUERY_INFORMATION;
        let Ok(process) = OpenProcess(access, false, pid) else {
            return false;
        };

        let wide: Vec<u16> = dll
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let bytes = wide.len() * 2;
        let remote_path = VirtualAllocEx(process, None, bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
        if remote_path.is_null() {
            let _ = CloseHandle(process);
            return false;
        }
        if WriteProcessMemory(process, remote_path, wide.as_ptr() as *const c_void, bytes, None).is_err() {
            let _ = VirtualFreeEx(process, remote_path, 0, MEM_RELEASE);
            let _ = CloseHandle(process);
            return false;
        }

        let Some(local_k32) = module_base(GetCurrentProcess(), "kernel32.dll") else {
            return false;
        };
        let Some(remote_k32) = module_base(process, "kernel32.dll") else {
            return false;
        };
        let load_library = windows::Win32::System::LibraryLoader::GetProcAddress(
            windows::Win32::Foundation::HMODULE(local_k32 as *mut c_void),
            windows::core::PCSTR(c"LoadLibraryW".as_ptr() as *const u8),
        );
        let Some(load_library) = load_library else { return false };
        let remote_load = ((load_library as usize) - local_k32 + remote_k32) as *const c_void;

        let Ok(thread) = CreateRemoteThread(
            process,
            None,
            0,
            Some(std::mem::transmute(remote_load)),
            Some(remote_path),
            0,
            None,
        ) else {
            let _ = VirtualFreeEx(process, remote_path, 0, MEM_RELEASE);
            let _ = CloseHandle(process);
            return false;
        };
        let _ = WaitForSingleObject(thread, 5000);
        let mut hmod = 0u32;
        let _ = GetExitCodeThread(thread, &mut hmod);
        let _ = CloseHandle(thread);
        let _ = VirtualFreeEx(process, remote_path, 0, MEM_RELEASE);
        if hmod == 0 {
            let _ = CloseHandle(process);
            return false;
        }

        let Ok(local_hook) = windows::Win32::System::LibraryLoader::LoadLibraryW(&HSTRING::from(
            dll.to_string_lossy().to_string(),
        )) else {
            let _ = CloseHandle(process);
            return false;
        };
        let Some(local_start) = windows::Win32::System::LibraryLoader::GetProcAddress(
            local_hook,
            windows::core::PCSTR(c"ITNGStart".as_ptr() as *const u8),
        ) else {
            let _ = CloseHandle(process);
            return false;
        };
        let offset = (local_start as usize) - (local_hook.0 as usize);
        let remote_start = (hmod as usize + offset) as *const c_void;
        let Ok(thread2) = CreateRemoteThread(
            process,
            None,
            0,
            Some(std::mem::transmute(remote_start)),
            Some(hwnd.0 as *const c_void),
            0,
            None,
        ) else {
            let _ = CloseHandle(process);
            return false;
        };
        let _ = WaitForSingleObject(thread2, 5000);
        let _ = CloseHandle(thread2);
        let _ = CloseHandle(process);
        true
    }
}

/// 是否允许注入这个进程（配置白名单，空 = 允许所有）
pub fn enabled_for(proc_name: &str) -> bool {
    let cfg = crate::config::AppConfig::load();
    if !cfg.inject_enabled() {
        return false;
    }
    let list = cfg.inject_exes();
    list.is_empty() || list.iter().any(|e| e.eq_ignore_ascii_case(proc_name))
}

/// 问一次共享内存里的光标位置
pub fn caret() -> Option<RECT> {
    let view = shared()?;
    unsafe {
        let hwnd = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let proc_name = crate::process::name(hwnd).unwrap_or_default();
        if !enabled_for(&proc_name) {
            return None;
        }
        let already = ATTACHED.with(|s| s.borrow().contains(&pid));
        if !already {
            if !inject(pid, hwnd) {
                return None;
            }
            ATTACHED.with(|s| {
                s.borrow_mut().insert(pid);
            });
        }

        let seq = SEQ.with(|s| {
            let v = s.get().wrapping_add(1);
            s.set(v);
            v
        });
        let mut result = 0usize;
        let _ = SendMessageTimeoutW(
            hwnd,
            HOOK_QUERY_MSG,
            WPARAM(seq as usize),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            60,
            Some(&mut result),
        );
        let shared = &*view;
        if shared.seq != seq || shared.width <= 0 {
            return None;
        }
        Some(RECT {
            left: shared.left,
            top: shared.top,
            right: shared.left + shared.width,
            bottom: shared.top + shared.height,
        })
    }
}
