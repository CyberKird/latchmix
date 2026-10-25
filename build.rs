// Embeds the manifest (per-monitor DPI, themed controls) and the icon/version resource
// without any extra build dependency. The resource needs rc.exe from the Windows SDK;
// if it is missing the build still succeeds, only the .exe file icon is lost.
use std::{env, fs, path::PathBuf, process::Command};

fn find_rc() -> Option<PathBuf> {
    let root = PathBuf::from(env::var_os("ProgramFiles(x86)")?).join("Windows Kits/10/bin");
    let mut versions: Vec<_> = fs::read_dir(root).ok()?.flatten().map(|e| e.path()).collect();
    versions.sort();
    versions.into_iter().rev().map(|v| v.join("x64/rc.exe")).find(|p| p.exists())
}

fn main() {
    let dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{dir}/latchmix.manifest");
    for f in ["latchmix.manifest", "latchmix.rc", "assets/icon.ico"] {
        println!("cargo:rerun-if-changed={f}");
    }

    let res = PathBuf::from(env::var("OUT_DIR").unwrap()).join("latchmix.res");
    let built = find_rc()
        .and_then(|rc| Command::new(rc).current_dir(&dir).args(["/nologo", "/fo"]).arg(&res).arg("latchmix.rc").status().ok())
        .is_some_and(|s| s.success());
    if built {
        println!("cargo:rustc-link-arg-bins={}", res.display());
    } else {
        println!("cargo:warning=rc.exe not found or failed: building without the exe icon and version info");
    }
}
