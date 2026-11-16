//! `lemnos-layout-check`: the header comparison for the drivers whose layouts come from a vendor
//! header that is not in this repository.
//!
//! - No arguments: prints one line per variant, `not run: needs <header> from <source>`, and exits 0.
//!   This is the gate's run: the drivers' own assertions are compiled and tested there.
//! - `lemnos-layout-check <driver>[/<variant>] -I <dir> [-I <dir>...] [-D <macro>...] [--target <triple>]`:
//!   runs bindgen on the variant's wrapper with the header directories, and compares each recorded
//!   number with bindgen's reading. Prints each disagreement; exits 1 on any. `--bindgen <path>` names
//!   the bindgen command (default `bindgen`, version 0.73.2 here).
//!
//! The bindgen command is needed only for the header run, and clang's libraries with it.

#![allow(clippy::print_stdout, clippy::print_stderr)] // A command-line tool prints its result.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use std::collections::HashMap;

use lemnos_layout_check::{
    Assert, VARIANTS, Variant, match_pairs, parse_asserts, parse_consts, wrapper_for,
};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn find(selector: &str) -> Option<&'static Variant> {
    VARIANTS.iter().copied().find(|v| {
        let name = format!("{}/{}", v.driver, v.name);
        name == selector || (v.driver == selector && v.name == "default")
    })
}

/// bindgen's values for the pairs at `indices`: `Err` carries the first error clang gave.
fn evaluate(
    run: &Run,
    variant: &Variant,
    indices: &[usize],
) -> Result<HashMap<usize, i128>, String> {
    let dir = std::env::temp_dir().join(format!("lemnos-layout-check-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    let wrapper = dir.join("wrapper.h");
    std::fs::write(&wrapper, wrapper_for(variant, indices))
        .map_err(|error| format!("{}: {error}", wrapper.display()))?;
    let mut command = Command::new(&run.bindgen);
    command.arg(&wrapper).args([
        "--use-core",
        "--no-doc-comments",
        "--allowlist-var",
        "LF_[0-9]+",
        "--",
    ]);
    for include in &run.includes {
        command.arg("-I").arg(include);
    }
    for define in variant
        .defines
        .iter()
        .map(|d| d.to_string())
        .chain(run.defines.iter().cloned())
    {
        command.arg(format!("-D{define}"));
    }
    if let Some(target) = &run.target {
        command.args(["-target", target]);
    }
    let output = command.output().map_err(|error| {
        format!(
            "cannot run {}: {error} (bindgen 0.73.2 is needed)",
            run.bindgen
        )
    })?;
    let _ = std::fs::remove_dir_all(&dir);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let first = stderr
            .lines()
            .find(|l| l.contains("error"))
            .unwrap_or("bindgen failed")
            .trim()
            .to_string();
        return Err(first);
    }
    Ok(parse_consts(&String::from_utf8_lossy(&output.stdout)))
}

struct Run {
    selector: String,
    includes: Vec<String>,
    defines: Vec<String>,
    target: Option<String>,
    bindgen: String,
    header_version: Option<String>,
    evidence: bool,
}

fn parse_args(args: &[String]) -> Result<Run, String> {
    let mut run = Run {
        selector: String::new(),
        includes: Vec::new(),
        defines: Vec::new(),
        target: None,
        bindgen: "bindgen".to_string(),
        header_version: None,
        evidence: false,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "-I" => run.includes.push(value("-I")?),
            "-D" => run.defines.push(value("-D")?),
            "--target" => run.target = Some(value("--target")?),
            "--bindgen" => run.bindgen = value("--bindgen")?,
            "--header-version" => run.header_version = Some(value("--header-version")?),
            "--evidence" => run.evidence = true,
            other if other.starts_with('-') => return Err(format!("unknown option {other}")),
            other => {
                if !run.selector.is_empty() {
                    return Err(format!("one variant at a time: {other}"));
                }
                run.selector = other.to_string();
            }
        }
    }
    Ok(run)
}

fn list() -> ExitCode {
    for variant in VARIANTS {
        println!("layout-check: {}", variant.not_run_message());
    }
    ExitCode::SUCCESS
}

fn check(run: &Run) -> ExitCode {
    let Some(variant) = find(&run.selector) else {
        eprintln!(
            "layout-check: no variant {:?}; the variants are:",
            run.selector
        );
        for v in VARIANTS {
            eprintln!("  {}/{}", v.driver, v.name);
        }
        return ExitCode::from(2);
    };
    if run.includes.is_empty() {
        println!("layout-check: {}", variant.not_run_message());
        return ExitCode::SUCCESS;
    }
    // A header is compared only when it is the version the record was taken from. Anything else is
    // not checked: it neither passes nor fails. `--evidence` reports the values anyway, labelled.
    if !run.evidence {
        let given = run.header_version.as_deref().unwrap_or("unstated");
        if variant.record == "unknown" {
            println!(
                "layout-check: {}/{}: not compared: the record's header revision is not pinned in the repository, so no header can be matched to it",
                variant.driver, variant.name
            );
            return ExitCode::SUCCESS;
        }
        if given != variant.record {
            println!(
                "layout-check: {}/{}: not compared: host header is version {given}, the record is from {}",
                variant.driver, variant.name, variant.record
            );
            return ExitCode::SUCCESS;
        }
    }

    let mut asserts = Vec::new();
    for file in variant.layout {
        match std::fs::read_to_string(repo().join(file)) {
            Ok(text) => asserts.extend(parse_asserts(&text)),
            Err(error) => {
                eprintln!("layout-check: {file}: {error}");
                return ExitCode::from(2);
            }
        }
    }

    let matched = match match_pairs(variant, &asserts) {
        Ok(matched) => matched,
        Err(error) => {
            eprintln!("layout-check: {}/{}: {error}", variant.driver, variant.name);
            return ExitCode::from(2);
        }
    };
    let all: Vec<usize> = (0..variant.pairs.len()).collect();
    let header = match evaluate(run, variant, &all) {
        Ok(values) => values,
        Err(_) => {
            // One fact the header does not have fails the whole wrapper: ask about each fact alone.
            let mut values = HashMap::new();
            let mut missing = Vec::new();
            for index in &all {
                match evaluate(run, variant, &[*index]) {
                    Ok(value) => values.extend(value),
                    Err(error) => missing.push((*index, error)),
                }
            }
            let good: Vec<usize> = values.keys().copied().collect();
            let mut report = values;
            if !good.is_empty() {
                match evaluate(run, variant, &good) {
                    Ok(again) => report = again,
                    Err(error) => {
                        eprintln!(
                            "layout-check: {}/{}: the facts that compile alone fail together: {error}",
                            variant.driver, variant.name
                        );
                        return ExitCode::from(2);
                    }
                }
            }
            for (index, error) in missing {
                println!("  NOT IN THIS HEADER {}: {}", variant.pairs[index].0, error);
            }
            report
        }
    };
    let mut differences = Vec::new();
    let mut indices: Vec<usize> = header.keys().copied().collect();
    indices.sort_unstable();
    for index in indices {
        let (lhs, _) = variant.pairs[index];
        let Assert { value: layout, .. } = &matched[index];
        if header[&index] != *layout {
            differences.push((lhs, *layout, header[&index]));
        }
    }
    let agree = header.len() - differences.len();
    let missing = variant.pairs.len() - header.len();

    let facts = variant.pairs.len();
    let label = if run.evidence {
        "EVIDENCE ONLY, not a check"
    } else {
        "compared"
    };
    println!(
        "layout-check: {}/{}: {label}: {facts} facts against {} (header version {}): {agree} agree, {} differ, {missing} not in this header",
        variant.driver,
        variant.name,
        run.includes.join(", "),
        run.header_version.as_deref().unwrap_or("unstated"),
        differences.len()
    );
    for (lhs, layout, header) in &differences {
        println!("  DIFFERS {lhs}: the layout asserts {layout}, the header gives {header}");
    }
    for text in variant.unmapped {
        println!("  not checked by the tool: {text}");
    }
    if run.evidence || (differences.is_empty() && missing == 0) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return list();
    }
    match parse_args(&args) {
        Ok(run) => check(&run),
        Err(error) => {
            eprintln!("layout-check: {error}");
            ExitCode::from(2)
        }
    }
}
