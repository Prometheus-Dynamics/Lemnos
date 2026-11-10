//! The receiver as the driver sees it, and the Linux implementation over the media controller.

use std::os::fd::AsFd;

use lemnos_core::{Result, Status};
use lemnos_linux_sys::media::{
    media_v2_entity, media_v2_interface, media_v2_link, media_v2_pad, topology as read_topology,
    topology_counts,
};
use lemnos_linux_sys::v4l2::device::zeroed;
use lemnos_v4l2::media::{
    LinkFlags, MediaDevice, PadRef, Topology, find_media_devices, list_media_devices,
};

use crate::route::{self, IspGraph};

/// What the driver asks of the receiver's media graph. The real one is [`LinuxHardware`]; tests
/// use a stand-in.
pub trait Hardware: Send {
    /// The receiver's media graph, opening its media device if it isn't open.
    fn topology(&mut self) -> Result<Topology>;
    /// The receiver's media device model: its device name.
    fn model(&mut self) -> Result<String>;
    /// Turns a data link on or off.
    fn setup_link(&mut self, source: PadRef, sink: PadRef, enable: bool) -> Result<()>;
    /// Starts watching the data link with graph id `link`. Called after a route, so the graph
    /// read here is the one [`check`](Self::check) compares against.
    fn watch(&mut self, link: u32) -> Result<()>;
    /// Whether the watched link is still there and on; `IO` when it isn't, or the graph can't be
    /// read. Allocates nothing.
    fn check(&mut self) -> Result<()>;
    /// Drops the open media device, so the next call opens it again.
    fn close(&mut self);
    /// Every ISP's graph on this machine.
    fn isps(&mut self) -> Result<Vec<IspGraph>>;
}

/// Which receiver to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The receiver's media device model, or `None` to use the only receiver.
    device: Option<String>,
}

/// A media device model is at most 31 bytes (`media_device_info.model`).
const MAX_MODEL: usize = 31;

impl Settings {
    /// `INVALID_ARGUMENT` for a name no media device could have.
    pub fn new(device: Option<&str>) -> Result<Self> {
        if device.is_some_and(|d| d.is_empty() || d.len() > MAX_MODEL || d.contains('\0')) {
            return Err(Status::INVALID_ARGUMENT);
        }
        Ok(Self {
            device: device.map(str::to_owned),
        })
    }
}

/// Whether the device at hand is the receiver asked for.
fn is_wanted(settings: &Settings, media: &MediaDevice) -> bool {
    let Ok(info) = media.device_info() else {
        return false;
    };
    match &settings.device {
        Some(device) => info.model == *device,
        None => media.topology().is_ok_and(|t| route::is_receiver(&t)),
    }
}

fn status(error: &lemnos_v4l2::Error) -> Status {
    match error.errno() {
        Some(libc::ENOENT | libc::ENODEV) => Status::NO_DEVICE,
        Some(libc::EACCES | libc::EPERM) => Status::ACCESS,
        Some(libc::EBUSY) => Status::BUSY,
        Some(libc::EINVAL) => Status::INVALID_ARGUMENT,
        _ => Status::IO,
    }
}

/// The graph as `MEDIA_IOC_G_TOPOLOGY` fills it, sized once when a link is watched so that
/// reading it again allocates nothing.
#[derive(Default)]
struct Scratch {
    entities: Vec<media_v2_entity>,
    interfaces: Vec<media_v2_interface>,
    pads: Vec<media_v2_pad>,
    links: Vec<media_v2_link>,
}

/// Room past the counts read when sizing, so a graph that grows a little still fits.
const SPARE: usize = 16;

impl Scratch {
    fn sized(counts: [u32; 4]) -> Self {
        let n = |c: u32| (c as usize).saturating_add(SPARE);
        Self {
            entities: vec![zeroed(); n(counts[0])],
            interfaces: vec![zeroed(); n(counts[1])],
            pads: vec![zeroed(); n(counts[2])],
            links: vec![zeroed(); n(counts[3])],
        }
    }
}

/// Whether link `id` is among `links` and on.
pub fn link_on(links: &[media_v2_link], id: u32) -> bool {
    links
        .iter()
        .find(|l| l.id == id)
        .is_some_and(|l| LinkFlags(l.flags).contains(LinkFlags::ENABLED))
}

/// The receiver through its media device. Opened as it's needed, so a receiver not yet probed is
/// a failed call rather than a failed bind, and a call can succeed once it is.
pub struct LinuxHardware {
    settings: Settings,
    media: Option<MediaDevice>,
    watched: Option<(u32, Scratch)>,
}

impl LinuxHardware {
    #[must_use]
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            media: None,
            watched: None,
        }
    }

    fn media(&mut self) -> Result<&MediaDevice> {
        if self.media.is_none() {
            let mut found = list_media_devices()
                .into_iter()
                .filter_map(|path| MediaDevice::open(path).ok())
                .filter(|m| is_wanted(&self.settings, m));
            let media = match (found.next(), found.next()) {
                (Some(media), None) => media,
                (None, _) => return Err(Status::NO_DEVICE),
                // Two receivers and none named: the board has to say which.
                (Some(_), Some(_)) => return Err(Status::INVALID_ARGUMENT),
            };
            self.media = Some(media);
        }
        self.media.as_ref().ok_or(Status::NO_DEVICE)
    }
}

impl Hardware for LinuxHardware {
    fn topology(&mut self) -> Result<Topology> {
        self.media()?.topology().map_err(|e| status(&e))
    }

    fn model(&mut self) -> Result<String> {
        let info = self.media()?.device_info().map_err(|e| status(&e))?;
        Ok(info.model)
    }

    fn setup_link(&mut self, source: PadRef, sink: PadRef, enable: bool) -> Result<()> {
        let flags = if enable {
            LinkFlags::ENABLED
        } else {
            LinkFlags::empty()
        };
        self.media()?
            .setup_link(source, sink, flags)
            .map_err(|e| status(&e))
    }

    fn watch(&mut self, link: u32) -> Result<()> {
        let counts = topology_counts(self.media()?.as_fd()).map_err(|_| Status::IO)?;
        let scratch = Scratch::sized([
            counts.num_entities,
            counts.num_interfaces,
            counts.num_pads,
            counts.num_links,
        ]);
        self.watched = Some((link, scratch));
        self.check()
    }

    fn check(&mut self) -> Result<()> {
        let media = self.media.as_ref().ok_or(Status::IO)?;
        let (link, scratch) = self.watched.as_mut().ok_or(Status::IO)?;
        let read = read_topology(
            media.as_fd(),
            &mut scratch.entities,
            &mut scratch.interfaces,
            &mut scratch.pads,
            &mut scratch.links,
        )
        .map_err(|_| Status::IO)?;
        let links = scratch
            .links
            .get(..read.num_links as usize)
            .ok_or(Status::IO)?;
        if link_on(links, *link) {
            Ok(())
        } else {
            Err(Status::IO)
        }
    }

    fn close(&mut self) {
        self.watched = None;
        self.media = None;
    }

    fn isps(&mut self) -> Result<Vec<IspGraph>> {
        let mut out = Vec::new();
        for path in find_media_devices(|m| {
            m.topology()
                .is_ok_and(|t| t.entity_by_name(route::ISP_ENTITY).is_some())
        }) {
            let media = MediaDevice::open_read_only(&path).map_err(|e| status(&e))?;
            let name = media.device_info().map_err(|e| status(&e))?.driver;
            let topology = media.topology().map_err(|e| status(&e))?;
            out.push(IspGraph { name, topology });
        }
        Ok(out)
    }
}
