/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Commands related to Godot executable

use crate::godot_version::parse_godot_version;
use crate::header_gen::generate_rust_binding;
use crate::watch::StopWatch;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

// Note: CARGO_BUILD_TARGET_DIR and CARGO_TARGET_DIR are not set.
// OUT_DIR would be standing to reason, but it's an unspecified path that cannot be referenced by CI.
const GODOT_VERSION_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../target/godot-gen/godot_version.txt"
);
const JSON_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../target/godot-gen/extension_api.json"
);
const HEADER_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../target/godot-gen/gdextension_interface.h"
);
const RES_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/res");

pub fn load_gdextension_json(watch: &mut StopWatch) -> String {
    let json_path = Path::new(JSON_PATH);
    rerun_on_changed(json_path);

    let godot_bin = locate_godot_binary();
    rerun_on_changed(&godot_bin);
    watch.record("locate_godot");

    // Regenerate API JSON if first time or Godot version is different
    let version = read_godot_version(&godot_bin);
    // if !json_path.exists() || has_version_changed(&version) {
    dump_extension_api(&godot_bin, json_path);
    update_version_file(&version);

    watch.record("dump_json");
    // }

    let result = std::fs::read_to_string(json_path)
        .unwrap_or_else(|_| panic!("failed to open file {}", json_path.display()));

    watch.record("read_json_file");
    result
}

pub fn load_gdextension_header_rs(
    c_in_path: Option<&Path>,
    rust_out_path: &Path,
    watch: &mut StopWatch,
) {
    let c_header_path;

    if let Some(c_in_path) = c_in_path {
        // External C header file provided; we don't invoke Godot
        c_header_path = c_in_path;
    } else {
        // No external C header file: Godot binary is present, we use it to dump C header
        let godot_bin = locate_godot_binary();
        rerun_on_changed(&godot_bin);
        watch.record("locate_godot");

        // Regenerate API JSON if first time or Godot version is different
        let version = read_godot_version(&godot_bin);
        c_header_path = Path::new(HEADER_PATH);

        // if !c_header_path.exists() || has_version_changed(&version) {
        dump_header_file(&godot_bin, c_header_path);
        update_version_file(&version);
        // }
    };
    rerun_on_changed(c_header_path);

    watch.record("dump_header_c");

    let tweak_path = Path::new(RES_PATH).join("tweak.patch");
    patch_c_header(c_header_path, &tweak_path);
    generate_rust_binding(c_header_path, rust_out_path);

    watch.record("generate_header_rs");
}

#[allow(dead_code)]
fn has_version_changed(current_version: &str) -> bool {
    let version_path = Path::new(GODOT_VERSION_PATH);

    match std::fs::read_to_string(version_path) {
        Ok(last_version) => current_version != last_version,
        Err(_) => true,
    }
}

fn update_version_file(version: &str) {
    let version_path = Path::new(GODOT_VERSION_PATH);
    rerun_on_changed(version_path);

    std::fs::write(version_path, version)
        .unwrap_or_else(|_| panic!("write Godot version to file {}", version_path.display()));
}

fn read_godot_version(godot_bin: &Path) -> String {
    let output = Command::new(godot_bin)
        .arg("--version")
        .output()
        .unwrap_or_else(|_| {
            panic!(
                "failed to invoke Godot executable '{}'",
                godot_bin.display()
            )
        });

    let output = String::from_utf8(output.stdout).expect("convert Godot version to UTF-8");
    println!("Godot version: {output}");

    match parse_godot_version(&output) {
        Ok(parsed) => {
            assert_eq!(
                parsed.major,
                4,
                "Only Godot versions >= 4.0 are supported; found version {}.",
                output.trim()
            );

            parsed.full_string
        }
        Err(e) => {
            // Don't treat this as fatal error
            panic!("failed to parse Godot version '{output}': {e}")
        }
    }
}

fn dump_extension_api(godot_bin: &Path, out_file: &Path) {
    let cwd = out_file.parent().unwrap();
    std::fs::create_dir_all(cwd).unwrap_or_else(|_| panic!("create directory '{}'", cwd.display()));
    println!("Dump GDExtension API JSON to dir '{}'...", cwd.display());

    let mut cmd = Command::new(godot_bin);
    cmd.current_dir(cwd)
        .arg("--headless")
        .arg("--dump-extension-api");

    execute(cmd, "dump Godot header file");

    println!("Generated {}/gdextension_interface.h.", cwd.display());
}

fn dump_header_file(godot_bin: &Path, out_file: &Path) {
    let cwd = out_file.parent().unwrap();
    std::fs::create_dir_all(cwd).unwrap_or_else(|_| panic!("create directory '{}'", cwd.display()));
    println!("Dump GDExtension header file to dir '{}'...", cwd.display());

    let mut cmd = Command::new(godot_bin);
    cmd.current_dir(cwd)
        .arg("--headless")
        .arg("--dump-gdextension-interface");

    execute(cmd, "dump Godot JSON file");

    println!("Generated {}/extension_api.json.", cwd.display());
}

fn patch_c_header(c_header_path: &Path, tweak_path: &Path) {
    // The C header path *must* be passed in by the invoking crate, as the path cannot be relative to this crate.
    // Otherwise, it can be something like `/home/runner/.cargo/git/checkouts/gdext-76630c89719e160c/efd3b94/godot-bindings`.
    let cwd = c_header_path.parent().unwrap().parent().unwrap();
    println!("cwd: {}", cwd.display());

    // Note: patch must have paths relative to Git root (aka top-level dir), so cwd is root
    rerun_on_changed(tweak_path);

    let git = locate_git_binary();
    let mut cmd = Command::new(&git);
    cmd.current_dir(cwd).arg("apply").arg("-v").arg(tweak_path);

    let output = execute(cmd, "apply Git patch");
    let stderr = String::from_utf8(output.stderr).expect("convert Git patch output to UTF-8");

    // `git patch` returns 0 even if it skips a patch because it's not applicable -- treat this as error
    assert!(!stderr.contains("Skipped"), "Git patch was skipped");
}

fn locate_godot_binary() -> PathBuf {
    if let Ok(string) = std::env::var("GODOT4_BIN") {
        println!("Found GODOT4_BIN with path to executable: '{string}'");
        println!("cargo:rerun-if-env-changed=GODOT4_BIN");
        PathBuf::from(string)
    } else if let Ok(path) = which::which("godot4") {
        println!("Found 'godot4' executable in PATH: {}", path.display());
        path
    } else {
        panic!(
            "gdext with `custom-godot` feature requires 'godot4' executable or a GODOT4_BIN \
                 environment variable (with the path to the executable)."
        )
    }
}

fn locate_git_binary() -> PathBuf {
    if let Ok(string) = std::env::var("GIT_BIN") {
        println!("Found GIT_BIN with path to executable: '{string}'");
        PathBuf::from(string)
    } else if let Ok(path) = which::which("git") {
        println!("Found 'git' executable in PATH: {}", path.display());
        path
    } else {
        panic!(
            "gdext with `custom-godot` feature requires `git` executable or a GIT_BIN \
                 environment variable (with the path to the executable)."
        )
    }
}

fn execute(mut cmd: Command, error_message: &str) -> Output {
    let output = cmd
        .output()
        .unwrap_or_else(|_| panic!("failed to execute command: {error_message}"));

    if output.status.success() {
        println!(
            "[stdout] {}",
            String::from_utf8(output.stdout.clone()).unwrap()
        );
        println!(
            "[stderr] {}",
            String::from_utf8(output.stderr.clone()).unwrap()
        );
        println!("[status] {}", output.status);
        output
    } else {
        println!("[stdout] {}", String::from_utf8(output.stdout).unwrap());
        println!("[stderr] {}", String::from_utf8(output.stderr).unwrap());
        println!("[status] {}", output.status);
        panic!("command returned error: {error_message}");
    }
}

fn rerun_on_changed(path: &Path) {
    println!("cargo:rerun-if-changed={}", path.display());
}
