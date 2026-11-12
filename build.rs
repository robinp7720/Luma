use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    for variable in [
        "PKG_CONFIG_PATH",
        "PKG_CONFIG_LIBDIR",
        "PKG_CONFIG_SYSROOT_DIR",
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
    }

    // Cargo does not track native libraries automatically. Follow the unversioned
    // linker paths so an Evolution/EDS upgrade invalidates the cached helper even
    // when none of our Rust sources changed.
    for (package, library, directory) in [
        ("camel-1.2", "camel-1.2", "libdir"),
        ("libedataserver-1.2", "edataserver-1.2", "libdir"),
        ("evolution-mail-3.0", "email-engine", "privlibdir"),
    ] {
        let pc_dir = pkg_config_variable(package, "pcfiledir");
        println!("cargo:rerun-if-changed={pc_dir}/{package}.pc");
        let lib_dir = pkg_config_variable(package, directory);
        println!("cargo:rerun-if-changed={lib_dir}/lib{library}.so");
    }

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

fn pkg_config_variable(package: &str, variable: &str) -> String {
    let output = Command::new("pkg-config")
        .args([&format!("--variable={variable}"), package])
        .output()
        .expect("failed to run pkg-config");
    assert!(
        output.status.success(),
        "pkg-config failed for {package}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = String::from_utf8(output.stdout).expect("pkg-config output was not UTF-8");
    let value = value.trim();
    assert!(
        !value.is_empty(),
        "pkg-config returned no {variable} for {package}"
    );
    value.to_string()
}
