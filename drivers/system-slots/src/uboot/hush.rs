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
    /// The words a `setexpr.l name *ADDR` reads: the SoC's registers, as a test gives them.
    pub mem: BTreeMap<u64, u32>,
    set: BTreeSet<String>,
    /// The partitions the device has, as `(device, name)`: what `part start` finds.
    pub parts: BTreeSet<(String, String)>,
    /// How many `saveenv` lines ran (a save that the script reaches, not one it skips).
    pub saves: usize,
}

impl Run {
    pub fn new(env: Env) -> Self {
        Self {
            env,
            reads: BTreeSet::new(),
            mem: BTreeMap::new(),
            set: BTreeSet::new(),
            parts: BTreeSet::new(),
            saves: 0,
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
            self.saves += 1;
            return true;
        }
        if let Some(args) = text.strip_prefix("part start ") {
            // `part start <interface> <dev> <name> <varname>` (U-Boot cmd/part.c, do_part_info): true when a
            // partition of that name is on the device, and the variable gets its number.
            let words: Vec<String> = args.split_whitespace().map(|w| self.word(w)).collect();
            let [interface, dev, name, var] = words.as_slice() else {
                panic!("part start takes four words here: {text}");
            };
            assert_eq!(interface, "mmc", "the script asks the MMC device only");
            let found = self.parts.contains(&(dev.clone(), name.clone()));
            if found {
                self.set.insert(var.clone());
                self.env.insert(var.clone(), "1".into());
            }
            return found;
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
        // The numeric comparisons (U-Boot cmd/test.c): each side is read as a decimal number, and text
        // that is not one reads as 0, as simple_strtol leaves it.
        for (op, cmp) in [
            (" -eq ", i64::eq as fn(&i64, &i64) -> bool),
            (" -ne ", i64::ne),
            (" -lt ", i64::lt),
            (" -le ", i64::le),
            (" -gt ", i64::gt),
            (" -ge ", i64::ge),
        ] {
            if let Some((a, b)) = args.split_once(op) {
                let number = |w: String| w.trim().parse::<i64>().unwrap_or(0);
                return cmp(&number(self.word(a)), &number(self.word(b)));
            }
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
            } else if line.starts_with("echo ") {
                // A message: it has no effect on what boots, so the run passes over it.
            } else if *line == "saveenv" {
                if live(&frames) {
                    self.saves += 1;
                }
            } else if let Some(args) = line.strip_prefix("setexpr.l ") {
                if !live(&frames) {
                    continue;
                }
                let (name, src) = args.split_once(' ').expect("setexpr.l NAME *ADDR");
                let addr = self.word(src.trim().strip_prefix('*').expect("setexpr.l reads *ADDR"));
                let addr = u64::from_str_radix(addr.trim_start_matches("0x"), 16)
                    .expect("setexpr.l address");
                let value = *self
                    .mem
                    .get(&addr)
                    .unwrap_or_else(|| panic!("no test register at {addr:#x}"));
                self.set.insert(name.to_string());
                self.env.insert(name.to_string(), value.to_string());
            } else if let Some(args) = line.strip_prefix("setexpr ") {
                if !live(&frames) {
                    continue;
                }
                let mut words = args.split_whitespace();
                let name = words.next().expect("setexpr NAME A OP B");
                let a = self.word(words.next().expect("setexpr A"));
                let op = self.word(words.next().expect("setexpr OP"));
                let b = self.word(words.next().expect("setexpr B"));
                let a: u64 = a.parse().expect("setexpr A is a number");
                let b: u64 = b.parse().expect("setexpr B is a number");
                let value = match op.as_str() {
                    "+" => a + b,
                    "&" => a & b,
                    other => panic!("setexpr operator {other} is not run here"),
                };
                self.set.insert(name.to_string());
                self.env.insert(name.to_string(), value.to_string());
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
            } else if line.starts_with("echo ") {
                // A message: it changes no variable and decides nothing.
            } else if live(&frames) {
                panic!("a command this evaluator does not run: {line}");
            }
        }
        assert!(frames.is_empty(), "an if without its fi");
    }
}

/// The restart count's lines of the script (BB2): from the count's first `if` to the first pass's
/// `setenv lemnos_pass`, which is where the count stops.
pub fn count_lines(script: &str) -> Vec<&str> {
    let code = code(script);
    let first = code
        .iter()
        .position(|l| *l == r#"if test -z "${lemnos_restarts}"; then"#)
        .expect("the script has no restart count");
    let end = code
        .iter()
        .position(|l| *l == "setenv lemnos_pass")
        .expect("the script has no `setenv lemnos_pass`");
    code[first..end].to_vec()
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
    choose_with(script, env, &BTreeSet::new(), false)
}

/// [`choose`] on a device with the partitions `parts` (`(device, name)`, what `part start` finds), with the
/// save decision (`lemnos_envdev`, `part start`, `lemnos_saveable`) run first, as the script runs it before
/// its loop.
pub fn choose_on(script: &str, env: Env, parts: &BTreeSet<(String, String)>) -> ([String; 3], Run) {
    choose_with(script, env, parts, true)
}

fn choose_with(
    script: &str,
    mut env: Env,
    parts: &BTreeSet<(String, String)>,
    save_decision: bool,
) -> ([String; 3], Run) {
    // The restart count block sets lemnos_restarts before the rung reads it; a caller that gives no
    // count is a boot with none raised yet.
    env.entry("lemnos_restarts".to_string())
        .or_insert_with(|| "0".to_string());
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
    run.parts = parts.clone();
    if save_decision {
        let from = at(r#"if test -z "${lemnos_envdev}"; then"#);
        let to = at(r#"if test -z "${lemnos_env}"; then"#);
        run.run(&code[from..to]);
    }
    run.run(&code[first..for_at]);
    let after = run.env.clone();
    let mut dtbs = [String::new(), String::new(), String::new()];
    for (i, step) in ["trial", "default", "previous"].iter().enumerate() {
        let mut each = Run::new(after.clone());
        each.parts = parts.clone();
        each.preset("step", step);
        // The choice's own variables are set by the script, not the record.
        each.set.extend(run.set.iter().cloned());
        each.run(&code[for_at + 1..load_at]);
        dtbs[i] = each.env.get("lemnos_dtb").cloned().unwrap_or_default();
        run.reads.extend(each.reads);
    }
    (dtbs, run)
}
