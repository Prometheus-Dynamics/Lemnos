//! Finding an I2C adapter by what it is rather than by its number, which
//! depends on probe order: its adapter name, its device-tree node's
//! `compatible`, its device-tree path or node name.

use lemnos_drivers_linux::{SysRoot, sysfs};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// What identifies an I2C adapter. Every given field must match; at least
/// one is given. Written in a board definition as
/// `bus = "i2c:<key>=<value>[;<key>=<value>...]"`, keys:
///
/// - `name`: the adapter's `name` attribute (`/sys/bus/i2c/devices/i2c-N/name`);
/// - `compatible`: one of the `compatible` strings of the adapter's
///   device-tree node (or its parent device's), such as `i2c-gpio` or
///   `snps,designware-i2c`;
/// - `of`: the device-tree path of that node (`/axi/pcie@1000120000/rp1/i2c@74000`);
/// - `node`: the last component of that path (`i2c@74000`, a DesignWare
///   adapter at a given address).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct I2cSelector {
    pub name: Option<String>,
    pub compatible: Option<String>,
    pub of: Option<String>,
    pub node: Option<String>,
}

impl I2cSelector {
    /// Parses the part after `i2c:`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut selector = Self::default();
        for part in text.split(';').filter(|p| !p.is_empty()) {
            let (key, value) = part
                .split_once('=')
                .ok_or_else(|| format!("{part:?}: expected <key>=<value>"))?;
            let value = Some(value.to_string());
            match key {
                "name" => selector.name = value,
                "compatible" => selector.compatible = value,
                "of" => selector.of = value,
                "node" => selector.node = value,
                other => {
                    return Err(format!(
                        "unknown I2C adapter key {other:?} (name, compatible, of, node)"
                    ));
                }
            }
        }
        if selector == Self::default() {
            return Err("an I2C adapter selector needs name, compatible, of or node".into());
        }
        Ok(selector)
    }

    /// The bus number of the one adapter under `sys` that matches; an
    /// error when none or several do.
    pub fn resolve(&self, sys: &SysRoot) -> Result<u32, String> {
        let mut found = Vec::new();
        for adapter in sysfs::entries(&sys.path().join("bus/i2c/devices")).unwrap_or_default() {
            let Some(bus) = adapter
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_prefix("i2c-"))
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            if self.matches(&adapter) {
                found.push(bus);
            }
        }
        match found[..] {
            [bus] => Ok(bus),
            [] => Err(format!("no I2C adapter matches {self}")),
            _ => Err(format!(
                "several I2C adapters match {self}: {}",
                found
                    .iter()
                    .map(|b| format!("i2c-{b}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    fn matches(&self, adapter: &Path) -> bool {
        if let Some(name) = &self.name {
            let found = sysfs::read_optional(&adapter.join("name")).ok().flatten();
            if found.as_deref() != Some(name.as_str()) {
                return false;
            }
        }
        if self.compatible.is_none() && self.of.is_none() && self.node.is_none() {
            return true;
        }
        let Some(node) = of_node(adapter) else {
            return false;
        };
        if let Some(compatible) = &self.compatible {
            let listed = fs::read(node.join("compatible")).unwrap_or_default();
            let any = listed
                .split(|b| *b == 0 || *b == b'\n')
                .any(|c| c == compatible.as_bytes());
            if !any {
                return false;
            }
        }
        let path = dt_path(&node);
        if let Some(of) = &self.of
            && path.as_deref() != Some(of.trim_end_matches('/'))
        {
            return false;
        }
        if let Some(wanted) = &self.node {
            let last = path.as_deref().and_then(|p| p.rsplit('/').next());
            if last != Some(wanted.as_str()) {
                return false;
            }
        }
        true
    }
}

/// The adapter's device-tree node: its own `of_node`, else its parent
/// device's (an adapter directory sits under the controller's device).
fn of_node(adapter: &Path) -> Option<PathBuf> {
    let own = adapter.join("of_node");
    if let Ok(node) = fs::canonicalize(&own) {
        return Some(node);
    }
    let resolved = fs::canonicalize(adapter).ok()?;
    fs::canonicalize(resolved.parent()?.join("of_node")).ok()
}

/// A resolved `of_node` as a device-tree path: what follows
/// `firmware/devicetree/base` (`/axi/pcie@1000120000/rp1/i2c@74000`).
fn dt_path(node: &Path) -> Option<String> {
    let text = node.to_str()?;
    let (_, rest) = text.split_once("firmware/devicetree/base")?;
    Some(if rest.is_empty() {
        "/".into()
    } else {
        rest.into()
    })
}

impl fmt::Display for I2cSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("i2c:")?;
        let mut first = true;
        for (key, value) in [
            ("name", &self.name),
            ("compatible", &self.compatible),
            ("of", &self.of),
            ("node", &self.node),
        ] {
            if let Some(value) = value {
                if !first {
                    f.write_str(";")?;
                }
                first = false;
                write!(f, "{key}={value}")?;
            }
        }
        Ok(())
    }
}
