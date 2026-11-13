//! The service's environment.

use std::path::PathBuf;
use std::time::Duration;

use orion::Address;

/// lemnosd's socket, as `lemnos-ctl` finds it.
pub const DEFAULT_LEMNOSD_SOCKET: &str = "/run/lemnos/lemnosd.sock";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub lemnosd_socket: PathBuf,
    /// The comm core to publish to: the board's own socket unless `ORION_ADDRESS` says.
    pub orion: Address,
    /// The node the devices must be published on, when `ORION_NODE_ID` names one. A comm core
    /// that welcomes this service as any other node gets nothing.
    pub node_id: Option<String>,
    /// Who the service runs as, when it starts as root: `LEMNOS_ORION_USER`. `None` runs it as it
    /// was started.
    pub run_as: Option<RunAs>,
    /// Devices that take files sent through Orion, and the directory each one's go into.
    pub files: Vec<(String, PathBuf)>,
    /// USB function devices whose bulk endpoints are offered to the comm core, each as the link
    /// it is for: `LEMNOS_ORION_LINKS`.
    pub links: Vec<Link>,
    /// How long a file or stream may go without a chunk before it is dropped, with the device job
    /// it feeds. The default is the files' idle bound, [`crate::files::IDLE`].
    pub stall: Duration,
    /// How long a caller's holds last after its last silence: `LEMNOS_ORION_LEASE`, in whole seconds.
    /// `None` when it is 0: no lease, and the bridge says nothing about one. See [`crate::calls`].
    pub lease: Option<Duration>,
    /// The files each device takes as a stream, fed to its long job as they arrive
    /// (`LEMNOS_ORION_STREAMS`).
    pub streams: Vec<(String, Vec<String>)>,
}

/// A device's bulk endpoints offered to the comm core as a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The device's name in the board file.
    pub device: String,
    /// The link the comm core knows it by.
    pub link: String,
}

/// The unprivileged identity the service drops to: a user, a primary group, and the supplementary
/// groups it needs (the daemon's group for its socket, and the Orion node's group).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunAs {
    /// The user, by name or number (`LEMNOS_ORION_USER`).
    pub user: String,
    /// The primary group, by name or number (`LEMNOS_ORION_GROUP`); the user's own when unset.
    pub group: Option<String>,
    /// The supplementary groups, by name or number (`LEMNOS_ORION_GROUPS`, comma-separated).
    pub groups: Vec<String>,
}

impl Config {
    /// Reads `LEMNOSD_SOCKET`, `ORION_ADDRESS`, `ORION_NODE_ID`, `LEMNOS_ORION_USER`, `_GROUP`, `_GROUPS` and
    /// `LEMNOS_ORION_FILES`.
    ///
    /// # Errors
    ///
    /// When `ORION_ADDRESS` is not an address, or `LEMNOS_ORION_FILES` isn't a list of
    /// `device=directory`.
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            lemnosd_socket: std::env::var_os("LEMNOSD_SOCKET")
                .map_or_else(|| PathBuf::from(DEFAULT_LEMNOSD_SOCKET), PathBuf::from),
            orion: address(std::env::var("ORION_ADDRESS").ok().as_deref())?,
            node_id: node_id(std::env::var("ORION_NODE_ID").ok().as_deref()),
            run_as: run_as(
                std::env::var("LEMNOS_ORION_USER").ok().as_deref(),
                std::env::var("LEMNOS_ORION_GROUP").ok().as_deref(),
                std::env::var("LEMNOS_ORION_GROUPS").ok().as_deref(),
            ),
            files: files(std::env::var("LEMNOS_ORION_FILES").ok().as_deref())?,
            links: links(std::env::var("LEMNOS_ORION_LINKS").ok().as_deref())?,
            streams: streams(std::env::var("LEMNOS_ORION_STREAMS").ok().as_deref())?,
            lease: lease(std::env::var("LEMNOS_ORION_LEASE").ok().as_deref())?,
            stall: crate::files::IDLE,
        })
    }
}

/// The identity to drop to, from the three variables. A user is needed for any of them to count.
pub fn run_as(user: Option<&str>, group: Option<&str>, groups: Option<&str>) -> Option<RunAs> {
    let user = user.filter(|value| !value.is_empty())?;
    let groups = groups
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect();
    Some(RunAs {
        user: user.to_owned(),
        group: group.filter(|value| !value.is_empty()).map(str::to_owned),
        groups,
    })
}

/// `ORION_ADDRESS` as an address. Unset or empty means the board's own comm core.
///
/// # Errors
///
/// When it is set and is not an address.
pub fn address(value: Option<&str>) -> Result<Address, String> {
    match value {
        Some(text) if !text.is_empty() => text
            .parse()
            .map_err(|error| format!("ORION_ADDRESS {text:?}: {error}")),
        _ => Ok(Address::default()),
    }
}

/// `ORION_NODE_ID` as the node to publish on. Unset or empty means whichever node the comm
/// core is.
pub fn node_id(value: Option<&str>) -> Option<String> {
    value.filter(|id| !id.is_empty()).map(str::to_owned)
}

/// `LEMNOS_ORION_FILES` as devices and their directories: `device=/a/dir`, comma separated.
/// Unset or empty means no device takes files.
///
/// # Errors
///
/// When an entry isn't `device=absolute directory`.
pub fn files(value: Option<&str>) -> Result<Vec<(String, PathBuf)>, String> {
    let Some(text) = value.filter(|text| !text.trim().is_empty()) else {
        return Ok(Vec::new());
    };
    text.split(',')
        .map(str::trim)
        .map(|entry| {
            let (device, dir) = entry
                .split_once('=')
                .filter(|(device, dir)| !device.is_empty() && dir.starts_with('/'))
                .ok_or_else(|| {
                    format!("LEMNOS_ORION_FILES {entry:?}: want device=/absolute/directory")
                })?;
            Ok((device.to_owned(), PathBuf::from(dir)))
        })
        .collect()
}

/// The streams each device takes: `device=name+name,device=name`, each name a file name the device's
/// directory takes (one path part of `[A-Za-z0-9._-]`, not starting with a dot).
///
/// # Errors
///
/// When an entry is not `device=names`, or a name is not a file name.
pub fn streams(value: Option<&str>) -> Result<Vec<(String, Vec<String>)>, String> {
    let Some(text) = value.filter(|text| !text.trim().is_empty()) else {
        return Ok(Vec::new());
    };
    text.split(',')
        .map(str::trim)
        .map(|entry| {
            let (device, names) = entry
                .split_once('=')
                .filter(|(device, names)| !device.is_empty() && !names.is_empty())
                .ok_or_else(|| format!("LEMNOS_ORION_STREAMS {entry:?}: want device=name+name"))?;
            let names: Vec<String> = names.split('+').map(str::to_owned).collect();
            if let Some(bad) = names.iter().find(|n| !crate::files::valid_name(n)) {
                return Err(format!(
                    "LEMNOS_ORION_STREAMS names {bad:?}, which isn't a file name"
                ));
            }
            Ok((device.to_owned(), names))
        })
        .collect()
}

/// `device=link` pairs, comma-separated, from `LEMNOS_ORION_LINKS`.
/// The lease when `LEMNOS_ORION_LEASE` is unset: a caller's holds last a minute after its last request.
pub const LEASE: Duration = Duration::from_secs(60);

/// `LEMNOS_ORION_LEASE`: whole seconds. Unset is [`LEASE`]; `0` turns the lease off (`None`).
///
/// # Errors
///
/// When the value is not a whole number of seconds.
pub fn lease(value: Option<&str>) -> Result<Option<Duration>, String> {
    let Some(text) = value.filter(|text| !text.trim().is_empty()) else {
        return Ok(Some(LEASE));
    };
    match text.trim().parse::<u64>() {
        Ok(0) => Ok(None),
        Ok(seconds) => Ok(Some(Duration::from_secs(seconds))),
        _ => Err(format!(
            "LEMNOS_ORION_LEASE {text:?}: want whole seconds (0 turns the lease off)"
        )),
    }
}

pub fn links(value: Option<&str>) -> Result<Vec<Link>, String> {
    let Some(text) = value.filter(|text| !text.trim().is_empty()) else {
        return Ok(Vec::new());
    };
    text.split(',')
        .map(str::trim)
        .map(|entry| {
            let (device, link) = entry
                .split_once('=')
                .filter(|(device, link)| !device.is_empty() && !link.is_empty())
                .ok_or_else(|| format!("LEMNOS_ORION_LINKS {entry:?}: want device=link"))?;
            Ok(Link {
                device: device.to_owned(),
                link: link.to_owned(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lease_is_whole_seconds_with_a_minute_by_default() {
        assert_eq!(lease(None), Ok(Some(LEASE)));
        assert_eq!(lease(Some("  ")), Ok(Some(LEASE)));
        assert_eq!(lease(Some("5")), Ok(Some(Duration::from_secs(5))));
        assert_eq!(lease(Some("0")), Ok(None), "0 turns the lease off");
        assert!(lease(Some("1.5")).is_err());
        assert!(lease(Some("soon")).is_err());
    }

    #[test]
    fn streams_name_the_files_each_device_takes_as_a_stream() {
        assert_eq!(streams(None), Ok(Vec::new()));
        assert_eq!(
            streams(Some("flash=image.img.xz+image.img")),
            Ok(vec![(
                "flash".to_owned(),
                vec!["image.img.xz".to_owned(), "image.img".to_owned()]
            )])
        );
        assert!(streams(Some("flash")).is_err());
        assert!(streams(Some("flash=../x")).is_err());
        assert!(streams(Some("=image.img")).is_err());
    }

    #[test]
    fn links_name_each_usb_device_and_its_link() {
        assert_eq!(links(None), Ok(Vec::new()));
        assert_eq!(
            links(Some("gadget=serial, other=link2")),
            Ok(vec![
                Link {
                    device: "gadget".to_owned(),
                    link: "serial".to_owned(),
                },
                Link {
                    device: "other".to_owned(),
                    link: "link2".to_owned(),
                },
            ])
        );
        assert!(links(Some("gadget")).is_err());
        assert!(links(Some("=serial")).is_err());
        assert!(links(Some("gadget=")).is_err());
    }

    #[test]
    fn files_name_each_device_and_its_directory() {
        assert_eq!(files(None), Ok(Vec::new()));
        assert_eq!(files(Some(" ")), Ok(Vec::new()));
        assert_eq!(
            files(Some("slots=/data/slots/.incoming, other=/x")),
            Ok(vec![
                ("slots".to_owned(), PathBuf::from("/data/slots/.incoming")),
                ("other".to_owned(), PathBuf::from("/x")),
            ])
        );
        assert!(files(Some("slots")).is_err());
        assert!(files(Some("slots=relative")).is_err());
        assert!(files(Some("=/x")).is_err());
    }

    #[test]
    fn an_unset_or_empty_node_id_is_any_node() {
        assert_eq!(node_id(None), None);
        assert_eq!(node_id(Some("")), None);
    }

    #[test]
    fn run_as_needs_a_user_and_splits_the_groups() {
        assert_eq!(run_as(None, Some("lemnos"), Some("lemnos")), None);
        assert_eq!(run_as(Some(""), None, None), None);
        assert_eq!(
            run_as(
                Some("lemnos-orion"),
                Some("lemnos-orion"),
                Some(" lemnos, orion ,,")
            ),
            Some(RunAs {
                user: "lemnos-orion".to_owned(),
                group: Some("lemnos-orion".to_owned()),
                groups: vec!["lemnos".to_owned(), "orion".to_owned()],
            })
        );
    }

    #[test]
    fn a_set_node_id_is_used_as_given() {
        assert_eq!(node_id(Some("node-a")).as_deref(), Some("node-a"));
        assert_eq!(node_id(Some(" node-a")).as_deref(), Some(" node-a"));
    }

    #[test]
    fn an_unset_address_is_the_boards_own_socket() {
        assert_eq!(address(None), Ok(Address::default()));
        assert_eq!(address(Some("")), Ok(Address::default()));
        assert_eq!(
            address(Some("/tmp/o.sock")),
            Ok(Address::Local(PathBuf::from("/tmp/o.sock")))
        );
        assert!(address(Some("nope://x")).is_err());
    }
}
