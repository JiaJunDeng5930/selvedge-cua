use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn main() {
    println!("cargo:rerun-if-changed=formal");
    println!("cargo:rerun-if-env-changed=ROCQ2RUST_PLUGIN");
    println!("cargo:rerun-if-env-changed=OCAMLPATH");
    println!("cargo:rerun-if-env-changed=PATH");

    let project = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let plugin = env::var_os("ROCQ2RUST_PLUGIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| project.join("../../../rocq2rust/_build/rocq2rust-plugin"));
    let plugin = plugin.canonicalize().unwrap_or_else(|error| {
        panic!(
            "Rocq2Rust plugin at {}: {error}. Set ROCQ2RUST_PLUGIN to the built plugin directory.",
            plugin.display()
        )
    });
    let findlib = plugin.join("findlib");
    for path in [&findlib, &plugin.join("rocq")] {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    for file in fs::read_dir(project.join("formal")).unwrap() {
        let file = file.unwrap();
        if file
            .path()
            .extension()
            .is_some_and(|extension| extension == "v")
        {
            fs::copy(file.path(), out.join(file.file_name())).unwrap();
        }
    }

    // The proof audit and export share these checked modules.
    let mut modules = Vec::new();
    for file in fs::read_dir(project.join("formal")).unwrap() {
        let file = file.unwrap().file_name();
        let file = file.to_str().unwrap();
        if let Some(module) = file.strip_suffix(".v")
            && !matches!(module, "ProofAssumptions" | "ExtractRules")
        {
            run(&out, Command::new("rocq").args(["compile", "-q", file]));
            modules.push(module.to_owned());
        }
    }
    run(
        &out,
        Command::new("rocq")
            .args(["check", "-silent"])
            .args(modules),
    );
    let audit = run(
        &out,
        Command::new("rocq").args(["compile", "-q", "ProofAssumptions.v"]),
    );
    let assumptions = String::from_utf8(audit.stdout).unwrap();
    assert!(
        !assumptions.trim().is_empty()
            && assumptions
                .lines()
                .all(|line| line == "Closed under the global context"),
        "Input rule proof assumptions:\n{assumptions}"
    );

    let previous = env::var_os("OCAMLPATH");
    let mut search = vec![findlib.clone()];
    if let Some(previous) = &previous {
        search.extend(env::split_paths(previous));
    }
    run(
        &out,
        Command::new("rocq")
            .env("OCAMLPATH", env::join_paths(search).unwrap())
            .args(["compile", "-q", "-I"])
            .arg(findlib)
            .arg("-Q")
            .arg(plugin.join("rocq"))
            .args(["Rocq2Rust", "ExtractRules.v"]),
    );
    let generated = fs::read_to_string(out.join("rules.rs")).unwrap();
    // Scope lints to extracted modules; hand-written adapters stay linted.
    let bindings = generated.lines().map(|line| {
        if line.starts_with("pub(crate) mod ") {
            format!(
                "#[allow(dead_code, unused, unreachable_patterns, nonstandard_style, clippy::all)]\n{line}"
            )
        } else {
            line.to_owned()
        }
    }).collect::<Vec<_>>().join("\n");
    fs::write(out.join("bindings.rs"), bindings).unwrap();
}

fn run(directory: &Path, command: &mut Command) -> Output {
    let output = command
        .current_dir(directory)
        .output()
        .unwrap_or_else(|error| panic!("Unable to run {command:?}: {error}"));
    assert!(
        output.status.success(),
        "{command:?} failed ({})\n{}{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}
