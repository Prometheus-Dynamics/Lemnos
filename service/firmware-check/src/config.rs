//! The firmware text, followed as the firmware follows it: `include`, section filters, `os_prefix`.

/// Where the firmware's files come from (the boot partition).
pub trait Source {
    /// The file at `path` from the root, or `None` when it is not there.
    fn fetch(&mut self, path: &str) -> Option<Vec<u8>>;
}

/// The section filters that are on, as the firmware of one board evaluates them.
#[derive(Debug, Clone, Default)]
pub struct Filters {
    /// Model filters that are on (`[NAME]`), besides `[all]`. Any other model filter is off.
    pub on: Vec<String>,
    /// Whether this is a trial boot (`[tryboot]` on).
    pub trial: bool,
    /// The boot count `[boot_count...]` filters compare with.
    pub boot_count: u32,
}

impl Filters {
    /// Whether the section `[name]` is on, or `None` for a filter this check does not know.
    fn on(&self, name: &str) -> Option<bool> {
        let n = name.trim().to_ascii_lowercase();
        if n == "all" {
            return Some(true);
        }
        if n == "none" {
            return Some(false);
        }
        if n == "tryboot" {
            return Some(self.trial);
        }
        if let Some(rest) = n.strip_prefix("boot_count") {
            let rest = rest.trim();
            let (op, v) = rest.split_at(rest.find(|c: char| c.is_ascii_digit())?);
            let v: u32 = v.trim().parse().ok()?;
            let c = self.boot_count;
            return match op.trim() {
                ">" => Some(c > v),
                "<" => Some(c < v),
                "=" | "==" => Some(c == v),
                "&" => Some(c & v != 0),
                _ => None,
            };
        }
        if n.contains(['=', ':', ' ']) {
            return None;
        }
        Some(self.on.iter().any(|o| o.eq_ignore_ascii_case(&n)))
    }
}

/// What the firmware text sets, in the order it was read.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    /// `key=value` lines (the last of a key wins), `dtoverlay` aside.
    pub keys: Vec<(String, String)>,
    /// `dtoverlay=` lines, in order (name and parameters as written).
    pub overlays: Vec<String>,
    /// `initramfs NAME [followkernel|ADDR]`: the name.
    pub initramfs: Option<String>,
    /// What was read, line by line: the files, the filters and their state, unknown filters.
    pub trace: Vec<String>,
}

impl Settings {
    /// The last value of `key`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.keys
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Reads `file` and what it includes into `settings`. Includes nest at most 8 deep.
pub(crate) fn read(
    src: &mut dyn Source,
    file: &str,
    filters: &Filters,
    settings: &mut Settings,
    depth: usize,
) {
    let pad = "  ".repeat(depth);
    if depth > 8 {
        settings
            .trace
            .push(format!("{pad}{file}: includes nest past 8, not read"));
        return;
    }
    let Some(data) = src.fetch(file) else {
        settings.trace.push(format!("{pad}{file}: not there"));
        return;
    };
    settings
        .trace
        .push(format!("{pad}{file}: {} bytes", data.len()));
    let text = String::from_utf8_lossy(&data).to_string();
    let mut on = true;
    for raw in text.lines() {
        let line = raw
            .split('#')
            .next()
            .unwrap_or("")
            .trim()
            .trim_matches('\0');
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            on = match filters.on(name) {
                Some(v) => v,
                None => {
                    settings.trace.push(format!(
                        "{pad}  [{name}]: a filter this check does not know, taken as off"
                    ));
                    false
                }
            };
            settings.trace.push(format!(
                "{pad}  [{name}]: {}",
                if on { "on" } else { "off" }
            ));
            continue;
        }
        if !on {
            continue;
        }
        if let Some(inc) = line.strip_prefix("include ") {
            read(src, inc.trim(), filters, settings, depth + 1);
            continue;
        }
        if let Some(rest) = line.strip_prefix("initramfs ") {
            let name = rest.split_whitespace().next().unwrap_or("").to_string();
            settings.trace.push(format!("{pad}  initramfs {name}"));
            settings.initramfs = Some(name);
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            settings
                .trace
                .push(format!("{pad}  a line without '=': {line}"));
            continue;
        };
        let (k, v) = (k.trim().to_string(), v.trim().to_string());
        settings.trace.push(format!("{pad}  {k}={v}"));
        if k == "dtoverlay" {
            settings.overlays.push(v);
        } else {
            settings.keys.push((k, v));
        }
    }
}
