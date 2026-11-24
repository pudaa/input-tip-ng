//! 开机自启：单一来源、可自检、可自愈。
//!
//! 方案取舍（详见 docs/constraints.md）：
//! - 首选「当前用户登录时触发」的计划任务，通过 Task Scheduler COM 创建：
//!   不需要管理员、不弹 UAC、不经过 schtasks.exe 或 PowerShell。
//!   旧版把建任务塞进 %TEMP% 的 PowerShell 脚本，杀软/执行策略任一环节就能让它静默失败；
//!   计划任务也不受「任务管理器 -> 启动应用」开关（StartupApproved）影响。
//! - 兜底 HKCU\Software\Microsoft\Windows\CurrentVersion\Run，写入永不需要管理员，
//!   但可能被系统的启动应用开关禁用。
//! - 两者只启用一个，避免双份启动后被单实例互杀。

use std::path::{Path, PathBuf};

use windows::core::{BSTR, GUID, Interface, PCWSTR};
use windows::Win32::Foundation::{ERROR_SUCCESS, VARIANT_BOOL, WIN32_ERROR};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows::Win32::System::TaskScheduler::{
    IExecAction, ILogonTrigger, ITaskService, TASK_ACTION_EXEC, TASK_CREATE_OR_UPDATE,
    TASK_INSTANCES_IGNORE_NEW, TASK_LOGON_INTERACTIVE_TOKEN, TASK_RUNLEVEL_LUA,
    TASK_RUNLEVEL_TYPE, TASK_TRIGGER_LOGON, TASK_TRIGGER_TYPE2,
};
use windows::Win32::System::Variant::VARIANT;

use crate::config::AppConfig;
use crate::log;

pub const TASK_NAME: &str = "input-tip-ng";
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "input-tip-ng";
pub const AUTOSTART_ARG: &str = "--autostart";

/// {0F87369F-A4E5-4CFC-BD3E-73E6154572DD}
const CLSID_TASK_SCHEDULER: GUID = GUID::from_u128(0x0F87369F_A4E5_4CFC_BD3E_73E6154572DD);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Task,
    RunKey,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Task => "task",
            Method::RunKey => "run",
        }
    }

    pub fn parse(s: &str) -> Method {
        if s.eq_ignore_ascii_case("run") {
            Method::RunKey
        } else {
            Method::Task
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct TaskCheck {
    pub exists: bool,
    pub enabled: bool,
    pub path_ok: bool,
    pub args_ok: bool,
    pub trigger_ok: bool,
    pub runlevel_ok: bool,
    pub detail: String,
}

impl TaskCheck {
    pub fn healthy(&self) -> bool {
        self.exists && self.enabled && self.path_ok && self.args_ok && self.trigger_ok
    }
}

/// 当前 exe 的规范化路径（去掉 \\?\ 前缀，便于与任务中的登记值比较）
pub fn current_exe() -> PathBuf {
    let p = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("input-tip-ng.exe"));
    let s = p.display().to_string();
    let s = s.strip_prefix("\\\\?\\").unwrap_or(&s).to_string();
    PathBuf::from(s)
}

fn exe_dir(exe: &Path) -> String {
    exe.parent().map(|p| p.display().to_string()).unwrap_or_default()
}

fn same_path(a: &str, b: &str) -> bool {
    let na = a.trim().trim_matches('"').replace('/', "\\");
    let nb = b.trim().trim_matches('"').replace('/', "\\");
    na.eq_ignore_ascii_case(&nb)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn connect() -> Result<ITaskService, String> {
    unsafe {
        let service: ITaskService =
            CoCreateInstance(&CLSID_TASK_SCHEDULER, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| format!("创建 Task Scheduler 实例失败: {e}"))?;
        service
            .Connect(
                &VARIANT::default(),
                &VARIANT::default(),
                &VARIANT::default(),
                &VARIANT::default(),
            )
            .map_err(|e| format!("连接 Task Scheduler 服务失败: {e}"))?;
        Ok(service)
    }
}

fn current_user(service: &ITaskService) -> String {
    if let Ok(u) = unsafe { service.ConnectedUser() } {
        let s = u.to_string();
        if !s.is_empty() {
            return s;
        }
    }
    let name = std::env::var("USERNAME").unwrap_or_default();
    let domain = std::env::var("USERDOMAIN").unwrap_or_default();
    if domain.is_empty() {
        name
    } else {
        format!("{domain}\\{name}")
    }
}

/// 创建（或覆盖）登录触发的计划任务
pub fn install_task(exe: &Path, delay_secs: u32) -> Result<(), String> {
    unsafe {
        let service = connect()?;
        let user = current_user(&service);
        let root = service
            .GetFolder(&BSTR::from("\\"))
            .map_err(|e| format!("打开任务根目录失败: {e}"))?;

        let def = service.NewTask(0).map_err(|e| format!("新建任务定义失败: {e}"))?;

        if let Ok(reg) = def.RegistrationInfo() {
            let _ = reg.SetAuthor(&BSTR::from("input-tip-ng"));
            let _ = reg.SetDescription(&BSTR::from(
                "input-tip-ng 登录自启（由程序自身维护，删除后程序下次启动会自动重建）",
            ));
        }

        let principal = def.Principal().map_err(|e| e.to_string())?;
        principal
            .SetLogonType(TASK_LOGON_INTERACTIVE_TOKEN)
            .map_err(|e| format!("设置登录类型失败: {e}"))?;
        // LUA = 普通权限。旧版用 Highest 追求权限，反而依赖 UAC 与任务可用性，让自启更难成功。
        principal
            .SetRunLevel(TASK_RUNLEVEL_LUA)
            .map_err(|e| format!("设置运行级别失败: {e}"))?;
        if !user.is_empty() {
            let _ = principal.SetUserId(&BSTR::from(user.as_str()));
        }

        let settings = def.Settings().map_err(|e| e.to_string())?;
        let _ = settings.SetEnabled(VARIANT_BOOL(1));
        let _ = settings.SetStartWhenAvailable(VARIANT_BOOL(1));
        let _ = settings.SetDisallowStartIfOnBatteries(VARIANT_BOOL(0));
        let _ = settings.SetStopIfGoingOnBatteries(VARIANT_BOOL(0));
        let _ = settings.SetMultipleInstances(TASK_INSTANCES_IGNORE_NEW);
        let _ = settings.SetExecutionTimeLimit(&BSTR::from("PT0S"));
        let _ = settings.SetRestartInterval(&BSTR::from("PT1M"));
        let _ = settings.SetRestartCount(3);
        let _ = settings.SetAllowHardTerminate(VARIANT_BOOL(1));

        let triggers = def.Triggers().map_err(|e| e.to_string())?;
        let trigger = triggers
            .Create(TASK_TRIGGER_LOGON)
            .map_err(|e| format!("创建登录触发器失败: {e}"))?;
        let logon: ILogonTrigger = trigger.cast().map_err(|e| e.to_string())?;
        if !user.is_empty() {
            let _ = logon.SetUserId(&BSTR::from(user.as_str()));
        }
        // 登录后延迟启动：等资源管理器与输入法就绪，避免状态探测踩空
        let _ = logon.SetDelay(&BSTR::from(format!("PT{delay_secs}S")));
        let _ = logon.SetEnabled(VARIANT_BOOL(1));

        let actions = def.Actions().map_err(|e| e.to_string())?;
        let action = actions
            .Create(TASK_ACTION_EXEC)
            .map_err(|e| format!("创建启动动作失败: {e}"))?;
        let exec: IExecAction = action.cast().map_err(|e| e.to_string())?;
        exec.SetPath(&BSTR::from(exe.display().to_string()))
            .map_err(|e| format!("设置任务路径失败: {e}"))?;
        let _ = exec.SetArguments(&BSTR::from(AUTOSTART_ARG));
        let _ = exec.SetWorkingDirectory(&BSTR::from(exe_dir(exe)));

        root.RegisterTaskDefinition(
            &BSTR::from(TASK_NAME),
            &def,
            TASK_CREATE_OR_UPDATE.0,
            &VARIANT::default(),
            &VARIANT::default(),
            TASK_LOGON_INTERACTIVE_TOKEN,
            &VARIANT::default(),
        )
        .map_err(|e| format!("注册计划任务失败: {e}"))?;

        Ok(())
    }
}

pub fn check_task(exe: &Path) -> TaskCheck {
    let mut out = TaskCheck::default();
    unsafe {
        let service = match connect() {
            Ok(s) => s,
            Err(e) => {
                out.detail = e;
                return out;
            }
        };
        let root = match service.GetFolder(&BSTR::from("\\")) {
            Ok(r) => r,
            Err(e) => {
                out.detail = format!("打开任务根目录失败: {e}");
                return out;
            }
        };
        let task = match root.GetTask(&BSTR::from(TASK_NAME)) {
            Ok(t) => t,
            Err(_) => {
                out.detail = "计划任务不存在".to_string();
                return out;
            }
        };
        out.exists = true;
        out.enabled = task.Enabled().map(|v| v.0 != 0).unwrap_or(false);

        if let Ok(def) = task.Definition() {
            if let Ok(actions) = def.Actions() {
                let mut n = 0i32;
                let _ = actions.Count(&mut n);
                if n >= 1 {
                    if let Ok(a) = actions.get_Item(1) {
                        if let Ok(exec) = a.cast::<IExecAction>() {
                            let mut path_b = BSTR::new();
                            let mut args_b = BSTR::new();
                            let _ = exec.Path(&mut path_b);
                            let _ = exec.Arguments(&mut args_b);
                            let path = path_b.to_string();
                            let args = args_b.to_string();
                            out.path_ok = same_path(&path, &exe.display().to_string());
                            out.args_ok = args.contains(AUTOSTART_ARG);
                            if !out.path_ok {
                                out.detail = format!("任务指向的路径已过期: {path}");
                            }
                        }
                    }
                }
            }

            if let Ok(triggers) = def.Triggers() {
                let mut n = 0i32;
                let _ = triggers.Count(&mut n);
                let mut has_logon = false;
                for i in 1..=n {
                    if let Ok(t) = triggers.get_Item(i) {
                        let mut ty = TASK_TRIGGER_TYPE2(0);
                        if t.Type(&mut ty).is_ok() && ty == TASK_TRIGGER_LOGON {
                            has_logon = true;
                        }
                    }
                }
                out.trigger_ok = has_logon;
                if !has_logon {
                    out.detail = "缺少登录触发器".to_string();
                }
            }

            if let Ok(p) = def.Principal() {
                let mut rl = TASK_RUNLEVEL_TYPE(0);
                if p.RunLevel(&mut rl).is_ok() {
                    out.runlevel_ok = rl == TASK_RUNLEVEL_LUA;
                }
            }
        }
        if out.detail.is_empty() {
            out.detail = if out.healthy() { "计划任务正常".to_string() } else { "计划任务异常".to_string() };
        }
    }
    out
}

pub fn install_run(exe: &Path) -> Result<(), String> {
    let sub = wide(RUN_KEY);
    let val = wide(RUN_VALUE);
    let data = wide(&format!("\"{}\" {AUTOSTART_ARG}", exe.display()));
    let st: WIN32_ERROR = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(sub.as_ptr()),
            PCWSTR::from_raw(val.as_ptr()),
            REG_SZ.0,
            Some(data.as_ptr() as *const core::ffi::c_void),
            (data.len() * 2) as u32,
        )
    };
    if st == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("写入 HKCU Run 失败: win32 错误 {}", st.0))
    }
}

pub fn check_run(exe: &Path) -> Option<String> {
    let sub = wide(RUN_KEY);
    let val = wide(RUN_VALUE);
    let mut buf = [0u16; 1024];
    let mut cb = (buf.len() * 2) as u32;
    let st: WIN32_ERROR = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(sub.as_ptr()),
            PCWSTR::from_raw(val.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
            Some(&mut cb),
        )
    };
    if st != ERROR_SUCCESS {
        return None;
    }
    let len = (cb as usize / 2).saturating_sub(1);
    let value = String::from_utf16_lossy(&buf[..len]);
    if value.to_lowercase().contains(&exe.display().to_string().to_lowercase()) {
        Some(value)
    } else {
        // 记录了别的程序（旧版残留或手工添加），同样返回，交由上层判定并清理
        Some(value)
    }
}

pub fn remove_run() -> bool {
    let sub = wide(RUN_KEY);
    let val = wide(RUN_VALUE);
    let st: WIN32_ERROR = unsafe {
        RegDeleteKeyValueW(HKEY_CURRENT_USER, PCWSTR::from_raw(sub.as_ptr()), PCWSTR::from_raw(val.as_ptr()))
    };
    st == ERROR_SUCCESS
}

pub fn remove_task() -> bool {
    unsafe {
        let Ok(service) = connect() else { return false };
        let Ok(root) = service.GetFolder(&BSTR::from("\\")) else {
            return false;
        };
        root.DeleteTask(&BSTR::from(TASK_NAME), 0).is_ok()
    }
}

/// 按首选方式安装自启，失败时回退到另一种，并清理另一种的残留避免双启。
pub fn install_preferring(exe: &Path, delay_secs: u32, prefer: Method) -> Result<Method, String> {
    match prefer {
        Method::Task => match install_task(exe, delay_secs) {
            Ok(()) => {
                let _ = remove_run();
                Ok(Method::Task)
            }
            Err(err) => {
                log::warn("autostart", &format!("计划任务创建失败，回退 Run 键: {err}"));
                install_run(exe)?;
                let _ = remove_task();
                Ok(Method::RunKey)
            }
        },
        Method::RunKey => match install_run(exe) {
            Ok(()) => {
                let _ = remove_task();
                Ok(Method::RunKey)
            }
            Err(err) => {
                log::warn("autostart", &format!("Run 键写入失败，回退计划任务: {err}"));
                install_task(exe, delay_secs)?;
                let _ = remove_run();
                Ok(Method::Task)
            }
        },
    }
}

/// 显式安装（--install-autostart）：始终优先计划任务。
pub fn install(exe: &Path, delay_secs: u32) -> Result<Method, String> {
    install_preferring(exe, delay_secs, Method::Task)
}

pub fn uninstall() {
    let _ = remove_task();
    let _ = remove_run();
}

#[derive(Debug, Clone)]
pub struct Status {
    pub exe: PathBuf,
    pub method: Method,
    pub enabled: bool,
    pub healthy: bool,
    pub task: TaskCheck,
    pub run_value: Option<String>,
}

impl Status {
    pub fn describe(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("程序路径: {}\n", self.exe.display()));
        s.push_str(&format!(
            "配置方式: {}（启用={}）\n",
            self.method.as_str(),
            self.enabled
        ));
        s.push_str(&format!(
            "计划任务: {} | 存在={} 启用={} 路径匹配={} 参数匹配={} 登录触发={} 普通权限={}\n",
            self.task.detail,
            self.task.exists,
            self.task.enabled,
            self.task.path_ok,
            self.task.args_ok,
            self.task.trigger_ok,
            self.task.runlevel_ok
        ));
        match &self.run_value {
            Some(v) => s.push_str(&format!("HKCU Run: {v}\n")),
            None => s.push_str("HKCU Run: 无\n"),
        }
        s.push_str(&format!("综合健康: {}", if self.healthy { "是" } else { "否" }));
        s
    }
}

/// 读取当前状态（不做修改）
pub fn status(cfg: &AppConfig) -> Status {
    let exe = current_exe();
    let task = check_task(&exe);
    let run_value = check_run(&exe);
    let method = Method::parse(cfg.autostart_method());
    let enabled = cfg.autostart_enabled();

    let healthy = match method {
        Method::Task => task.healthy(),
        Method::RunKey => run_value
            .as_deref()
            .map(|v| v.to_lowercase().contains(&exe.display().to_string().to_lowercase()))
            .unwrap_or(false),
    };

    Status { exe, method, enabled, healthy, task, run_value }
}

/// 自检 + 自愈。返回是否健康；任何修复都会写日志。
pub fn verify_and_repair(cfg: &mut AppConfig) -> bool {
    let mut st = status(cfg);

    if !st.enabled {
        if st.task.exists || st.run_value.is_some() {
            uninstall();
            log::info("autostart", "配置为关闭自启，已清理残留登记");
        }
        return true;
    }

    // 双份登记会触发单实例互杀，收敛到配置指定的方式
    if st.task.exists && st.run_value.is_some() {
        log::warn("autostart", "检测到计划任务与 Run 键同时存在，清理多余项");
        match st.method {
            Method::Task => {
                let _ = remove_run();
            }
            Method::RunKey => {
                let _ = remove_task();
            }
        }
    }

    // 开发构建（target\debug|release）不登记自启：避免测试时把临时路径写进登录项
    let exe_text = st.exe.display().to_string().to_lowercase();
    if exe_text.contains("\\target\\debug\\") || exe_text.contains("\\target\\release\\") {
        log::info("autostart", "开发构建，跳过自启登记");
        return st.healthy;
    }

    if !st.healthy {
        log::warn("autostart", &format!("自启不健康，尝试修复：{}", st.task.detail));
        match install_preferring(&st.exe, cfg.autostart_delay_secs(), st.method) {
            Ok(m) => {
                cfg.set_autostart(true, m.as_str());
                let _ = cfg.save();
                log::info("autostart", &format!("自启已修复，方式={}", m.as_str()));
                st = status(cfg);
            }
            Err(e) => {
                log::error("autostart", &format!("自启修复失败: {e}"));
                return false;
            }
        }
    } else {
        log::info("autostart", "自启检查通过");
    }

    st.healthy
}
