use std::fs;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let version = env!("CARGO_PKG_VERSION");
    let target_triple = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("TARGET").ok())
        .unwrap_or_else(host_triple);

    let npm_platform = match target_triple.as_str() {
        "x86_64-pc-windows-msvc" => "win32-x64",
        "aarch64-pc-windows-msvc" => "win32-arm64",
        "x86_64-apple-darwin" => "darwin-x64",
        "aarch64-apple-darwin" => "darwin-arm64",
        "x86_64-unknown-linux-gnu" => "linux-x64",
        "aarch64-unknown-linux-gnu" => "linux-arm64",
        "x86_64-unknown-linux-musl" => "linux-x64-musl",
        _ => {
            eprintln!("Unknown target: {}", target_triple);
            std::process::exit(1);
        }
    };

    let exe_name = if target_triple.contains("windows") {
        "codecairn.exe"
    } else {
        "codecairn"
    };
    let src_bin = Path::new("target")
        .join(&target_triple)
        .join("release")
        .join(exe_name);
    let dest_dir = Path::new("npm/platforms").join(npm_platform).join("bin");
    let dest_bin = dest_dir.join(exe_name);

    fs::create_dir_all(&dest_dir)?;

    if src_bin.exists() {
        fs::copy(&src_bin, &dest_bin)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&dest_bin)?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&dest_bin, perms)?;
        }
        println!("Copied {} -> {}", src_bin.display(), dest_bin.display());
    } else {
        eprintln!("Source binary not found: {}", src_bin.display());
        std::process::exit(1);
    }

    // Update package.json version
    let pkg_path = Path::new("npm/platforms")
        .join(npm_platform)
        .join("package.json");
    let mut pkg: serde_json::Value = serde_json::from_str(&fs::read_to_string(&pkg_path)?)?;
    pkg["version"] = serde_json::Value::String(version.to_string());
    fs::write(&pkg_path, serde_json::to_string_pretty(&pkg)?)?;

    Ok(())
}

/// Best-effort mapping of the compiling host to a supportable rust target triple.
fn host_triple() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    match (os, arch) {
        ("windows", "x86_64") => "x86_64-pc-windows-msvc".to_string(),
        ("windows", "aarch64") => "aarch64-pc-windows-msvc".to_string(),
        ("macos", "aarch64") => "aarch64-apple-darwin".to_string(),
        ("macos", _) => "x86_64-apple-darwin".to_string(),
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu".to_string(),
        ("linux", _) => "x86_64-unknown-linux-gnu".to_string(),
        _ => "unknown".to_string(),
    }
}
