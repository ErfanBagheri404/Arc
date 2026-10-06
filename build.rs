//! Compiles `app.rc` (the Arc icon) into a COFF object and hands it to the
//! linker via the Windows SDK's `rc.exe` — no crate, no build-time dependency.
//!
//! If rc.exe is missing (a bare Build Tools install without the SDK) the build
//! still succeeds: the exe just carries the default Windows icon. The overlay
//! itself never depends on this resource existing.

use std::{env, path::PathBuf, process::Command};

fn find_rc() -> Option<PathBuf> {
    const KITS_ROOT: &str = r"C:\Program Files (x86)\Windows Kits\10\bin";
    // Newest SDK first; rc.exe output is identical across versions. Version
    // dirs look like "10.0.26100.0" — skip the arch dirs ("x64", "arm64", …)
    // by requiring a fully numeric name.
    let mut candidates: Vec<((u32, u32, u32), PathBuf)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(KITS_ROOT) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let nums = name
                .split('.')
                .map(str::parse::<u32>)
                .collect::<Result<Vec<u32>, _>>();
            let Ok(nums) = nums else { continue };
            if nums.len() < 3 {
                continue;
            }
            let x64 = entry.path().join("x64");
            if x64.join("rc.exe").exists() {
                candidates.push(((nums[0], nums[1], nums[2]), x64));
            }
        }
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    candidates.pop().map(|(_, dir)| dir.join("rc.exe"))
}

fn main() {
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=assets/arc-icon.ico");
    if env::var("CARGO_CFG_TARGET_OS").ok().as_deref() != Some("windows") {
        return;
    }
    let Some(rc) = find_rc() else {
        println!("cargo:warning=rc.exe not found — building without the Arc icon resource");
        return;
    };
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let res = out.join("arc-icon.res");
    let status = Command::new(&rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res)
        .arg("app.rc")
        .status();
    match status {
        Ok(s) if s.success() => println!("cargo:rustc-link-arg={}", res.display()),
        Ok(s) => println!("cargo:warning=rc.exe exited {s} — no icon resource"),
        Err(e) => println!("cargo:warning=rc.exe could not run ({e}) — no icon resource"),
    }
}
