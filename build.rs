//! 把图标资源编译进 exe，让资源管理器/快捷方式显示应用图标。
//!
//! rc.exe 随 Windows SDK 安装（不是 MSVC 目录），按版本号从新到旧找 x64 版；
//! 找不到就跳过，构建仍然通过。

use std::path::{Path, PathBuf};
use std::process::Command;

fn find_rc() -> Option<PathBuf> {
    let kits = Path::new(r"C:\\Program Files (x86)\\Windows Kits\\10\\bin");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(kits)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    versions.sort();
    for v in versions.iter().rev() {
        for arch in ["x64", "x86"] {
            let p = v.join(arch).join("rc.exe");
            if p.exists() {
                return Some(p);
            }
        }
    }
    // 退路：VS 自带的（部分安装会把 SDK 合并进 VC 目录）
    for root in [
        r"D:\\Program Files\\Microsoft Visual Studio\\2022\\Community",
        r"C:\\Program Files\\Microsoft Visual Studio\\2022\\Community",
    ] {
        let vc = Path::new(root).join("VC/Tools/MSVC");
        if let Ok(entries) = std::fs::read_dir(&vc) {
            let mut cands: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path().join("bin/Hostx64/x64/rc.exe"))
                .filter(|p| p.exists())
                .collect();
            cands.sort();
            if let Some(p) = cands.pop() {
                return Some(p);
            }
        }
    }
    None
}

fn main() {
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/app.ico");
    let Some(rc) = find_rc() else {
        println!("cargo:warning=未找到 rc.exe，跳过 exe 图标资源");
        return;
    };
    let out_dir = std::env::var("OUT_DIR").unwrap_or_else(|_| ".".into());
    let res = Path::new(&out_dir).join("app.res");
    let status = Command::new(&rc)
        .current_dir("assets")
        .args(["/nologo", "/fo"])
        .arg(&res)
        .arg("app.rc")
        .status();
    match status {
        Ok(s) if s.success() => println!("cargo:rustc-link-arg={}", res.display()),
        _ => println!("cargo:warning=rc.exe 编译图标资源失败，跳过"),
    }
}
