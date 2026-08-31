use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn run(command: &mut Command) {
    let status = command.status().expect("failed to start build command");
    if !status.success() {
        panic!("build command failed with status {status}");
    }
}

fn compile_cpp(src: &Path, obj: &Path, include_dirs: &[PathBuf]) {
    let mut cmd = Command::new("cl.exe");
    cmd.arg("/nologo")
        .arg("/EHsc")
        .arg("/std:c++14")
        .arg("/MD")
        .arg("/DWIN32")
        .arg("/D_WINDOWS")
        .arg("/DNOMINMAX")
        .arg("/c")
        .arg(src)
        .arg(format!("/Fo{}", obj.display()));

    for include_dir in include_dirs {
        cmd.arg(format!("/I{}", include_dir.display()));
    }

    run(&mut cmd);
}

fn main() {
    println!("cargo:rerun-if-env-changed=FRONTPANEL_SDK");
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_dir = manifest_dir.ancestors().nth(2).unwrap().to_path_buf();
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let intan_dir = workspace_dir
        .join("vendor")
        .join("intan-rhythmstim-api-230822");
    let frontpanel_sdk = PathBuf::from(
        env::var("FRONTPANEL_SDK")
            .expect("FRONTPANEL_SDK must point to the FrontPanelUSB SDK directory"),
    );

    let include_dirs = vec![
        manifest_dir.join("native"),
        intan_dir.clone(),
        frontpanel_sdk.join("API").join("include"),
    ];

    let sources = vec![
        manifest_dir.join("native").join("rhs_scan_bridge.cpp"),
        intan_dir.join("abstractrhxcontroller.cpp"),
        intan_dir.join("rhxcontroller.cpp"),
        intan_dir.join("rhxdatablock.cpp"),
        intan_dir.join("rhxregisters.cpp"),
    ];

    let mut objects = Vec::new();
    for src in &sources {
        println!("cargo:rerun-if-changed={}", src.display());
        let stem = src.file_stem().unwrap().to_string_lossy();
        let obj = out_dir.join(format!("{stem}.obj"));
        compile_cpp(src, &obj, &include_dirs);
        objects.push(obj);
    }

    let lib_path = out_dir.join("rhs_bridge.lib");
    let mut lib_cmd = Command::new("lib.exe");
    lib_cmd
        .arg("/nologo")
        .arg(format!("/OUT:{}", lib_path.display()));
    for obj in &objects {
        lib_cmd.arg(obj);
    }
    run(&mut lib_cmd);

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!(
        "cargo:rustc-link-search=native={}",
        frontpanel_sdk.join("API").join("lib").join("x64").display()
    );
    println!("cargo:rustc-link-lib=static=rhs_bridge");
    println!("cargo:rustc-link-lib=dylib=okFrontPanel");
}
