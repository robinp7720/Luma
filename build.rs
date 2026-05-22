use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let output = Command::new("pkg-config")
        .args([
            "--libs",
            "camel-1.2",
            "libedataserver-1.2",
            "evolution-mail-3.0",
        ])
        .output()
        .expect("failed to run pkg-config");

    if !output.status.success() {
        panic!(
            "pkg-config failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let mut rpaths = Vec::new();
    for token in String::from_utf8_lossy(&output.stdout).split_whitespace() {
        if let Some(path) = token.strip_prefix("-L") {
            println!("cargo:rustc-link-search=native={path}");
            continue;
        }

        if let Some(path) = token
            .strip_prefix("-Wl,-R")
            .or_else(|| token.strip_prefix("-Wl,-rpath,"))
            .or_else(|| token.strip_prefix("-Wl,-rpath="))
        {
            rpaths.extend(
                path.split(':')
                    .filter(|path| !path.is_empty())
                    .map(str::to_string),
            );
            continue;
        }
    }

    for rpath in rpaths {
        println!("cargo:rustc-link-arg-bin=luma-mail-eds=-Wl,-rpath,{rpath}");
    }
}
