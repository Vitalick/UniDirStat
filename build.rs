use std::{env, error::Error, fs, path::PathBuf, process::Command};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=assets/unidirstat.ico");
    println!("cargo:rerun-if-changed=packaging/windows/unidirstat.rc");
    println!("cargo:rerun-if-env-changed=RC");
    if env::var("CARGO_CFG_TARGET_OS")? != "windows" || env::var_os("CARGO_FEATURE_GUI").is_none() {
        return Ok(());
    }
    let msvc = env::var("CARGO_CFG_TARGET_ENV")? == "msvc";
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("Missing Cargo output directory")?)
        .join(if msvc { "icon.res" } else { "icon.o" });
    let compiler = env::var_os("RC").map(PathBuf::from).unwrap_or_else(|| {
        if !msvc {
            return PathBuf::from("windres");
        }
        let architecture = if env::var("HOST").is_ok_and(|host| host.starts_with("aarch64")) {
            "arm64"
        } else {
            "x64"
        };
        let sdk = env::var_os("WindowsSdkDir").map(PathBuf::from).or_else(|| {
            env::var_os("ProgramFiles(x86)").map(|root| PathBuf::from(root).join("Windows Kits/10"))
        });
        let mut compilers: Vec<_> = sdk
            .and_then(|root| fs::read_dir(root.join("bin")).ok())
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path().join(architecture).join("rc.exe"))
            .filter(|path| path.is_file())
            .collect();
        compilers.sort();
        compilers.pop().unwrap_or_else(|| PathBuf::from("rc.exe"))
    });
    let mut command = Command::new(compiler);
    if msvc {
        command.arg("/nologo").arg("/fo").arg(&output);
    } else {
        command.arg("-O").arg("coff").arg("-o").arg(&output);
    }
    let status = command.arg("packaging/windows/unidirstat.rc").status()?;
    if !status.success() {
        return Err("Windows icon resource compilation failed".into());
    }
    println!("cargo:rustc-link-arg-bin=unidirstat={}", output.display());
    Ok(())
}
