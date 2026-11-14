//! A small evaluator for the subset of U-Boot's hush that `packaging/uboot/boot.cmd` uses to choose
//! what to start: `if`/`elif`/`else`/`fi` over `test -n`, `test -z`, `test A = B` and `test A != B`,
//! `setenv`, `saveenv` (as a condition) and `${name}` expansion. It runs a range of the script's lines
//! over an environment, and records which variables the lines read from the environment (a name the
//! lines set themselves earlier is not one).
//!
//! What it does NOT share with U-Boot: hush's word splitting (every value here is one word, as the
//! record's values are), `sysboot`, `load` and `fdt` (a line using any other command is refused, so
//! the range must end before them), and the loader's own variables (the caller presets them).

use std::collections::{BTreeMap, BTreeSet};

pub type Env = BTreeMap<String, String>;

pub struct Run {
    pub env: Env,
    /// Names read from the environment the run began with, before the lines set them.
    pub reads: BTreeSet<String>,
    set: BTreeSet<String>,
}

impl Run {
    pub fn new(env: Env) -> Self {
        Self {
            env,
            reads: BTreeSet::new(),
            set: BTreeSet::new(),
        }
    }

    fn expand(&mut self, text: &str) -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some(at) = rest.find("${") {
            out.push_str(&rest[..at]);
            let end = rest[at..].find('}').expect("an unclosed ${") + at;
            let name = &rest[at + 2..end];
            if !self.set.contains(name) {
                self.reads.insert(name.to_string());
            }
            out.push_str(self.env.get(name).map_or("", String::as_str));
            rest = &rest[end + 1..];
        }
        out.push_str(rest);
        out
    }

    fn word(&mut self, text: &str) -> String {
        let text = text.trim();
        let inner = text
            .strip_prefix('"')
            .and_then(|t| t.strip_suffix('"'))
            .unwrap_or(text);
        self.expand(inner)
    }

    /// `test ...` or `saveenv` (which succeeds here).
    fn condition(&mut self, text: &str) -> bool {
        if text == "saveenv" {
            return true;
        }
        let args = text
            .strip_prefix("test ")
            .unwrap_or_else(|| panic!("not a condition this evaluator knows: {text}"));
        if let Some(word) = args.strip_prefix("-n ") {
            return !self.word(word).is_empty();
        }
        if let Some(word) = args.strip_prefix("-z ") {
            return self.word(word).is_empty();
        }
        if let Some((a, b)) = args.split_once(" != ") {
            return self.word(a) != self.word(b);
        }
        if let Some((a, b)) = args.split_once(" = ") {
            return self.word(a) == self.word(b);
        }
        panic!("not a test this evaluator knows: {text}")
    }

    /// Runs `lines` (trimmed, comments left out by the caller).
    pub fn run(&mut self, lines: &[&str]) {
        // One frame per open `if`: the branch before it was live, whether one branch has been taken,
        // and whether the current branch is live.
        let mut frames: Vec<(bool, bool, bool)> = Vec::new();
        let live = |frames: &[(bool, bool, bool)]| frames.last().is_none_or(|f| f.2);
        for line in lines {
            if let Some(cond) = line
                .strip_prefix("if ")
                .and_then(|l| l.strip_suffix("; then"))
            {
                let parent = live(&frames);
                let now = parent && self.condition(cond);
                frames.push((parent, now, now));
            } else if let Some(cond) = line
                .strip_prefix("elif ")
                .and_then(|l| l.strip_suffix("; then"))
            {
                let (parent, taken, _) = *frames.last().expect("elif without if");
                let now = parent && !taken && self.condition(cond);
                *frames.last_mut().unwrap() = (parent, taken || now, now);
            } else if *line == "else" {
                let (parent, taken, _) = *frames.last().expect("else without if");
                *frames.last_mut().unwrap() = (parent, true, parent && !taken);
            } else if *line == "fi" {
                frames.pop().expect("fi without if");
            } else if let Some(args) = line.strip_prefix("setenv ") {
                if !live(&frames) {
                    continue;
                }
                let (name, value) = args.split_once(' ').unwrap_or((args, ""));
                let value = self.word(value);
                self.set.insert(name.to_string());
                if value.is_empty() {
                    self.env.remove(name);
                } else {
                    self.env.insert(name.to_string(), value);
                }
            } else if live(&frames) {
                panic!("a command this evaluator does not run: {line}");
            }
        }
        assert!(frames.is_empty(), "an if without its fi");
    }
}

/// The script's code lines: trimmed, no comments or blank lines.
pub fn code(script: &str) -> Vec<&str> {
    script
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

impl Run {
    /// Sets a variable the loader (not the record) provides: a read of it is not a record read.
    pub fn preset(&mut self, name: &str, value: &str) {
        self.set.insert(name.to_string());
        self.env.insert(name.to_string(), value.to_string());
    }
}

/// The choice `boot.cmd` makes over `env`: the lines from `setenv left` (the first of the per-pass
/// variables) to the loop, run over the environment as stored; then the body of the loop up to its
/// first `load`, run for each of the three steps over what the choice set. Returns the three
/// `lemnos_dtb` values, trial, default and previous (empty: the step starts nothing), and the run
/// that holds every read of the environment.
pub fn choose(script: &str, env: Env) -> ([String; 3], Run) {
    let code = code(script);
    let at = |needle: &str| {
        code.iter()
            .position(|l| *l == needle)
            .unwrap_or_else(|| panic!("the script has no `{needle}`"))
    };
    let first = at("setenv left");
    let for_at = at("for step in trial default previous; do");
    let load_at = at(r#"if test -n "${lemnos_dtb}"; then"#);
    let mut run = Run::new(env);
    run.run(&code[first..for_at]);
    let after = run.env.clone();
    let mut dtbs = [String::new(), String::new(), String::new()];
    for (i, step) in ["trial", "default", "previous"].iter().enumerate() {
        let mut each = Run::new(after.clone());
        each.preset("step", step);
        // The choice's own variables are set by the script, not the record.
        each.set.extend(run.set.iter().cloned());
        each.run(&code[for_at + 1..load_at]);
        dtbs[i] = each.env.get("lemnos_dtb").cloned().unwrap_or_default();
        run.reads.extend(each.reads);
    }
    (dtbs, run)
}
