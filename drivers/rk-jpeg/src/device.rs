//! What the session needs of an open `/dev/mpp_service` file, and the one that provides it:
//! the shared service layer's session (`lemnos-driver-rk-mpp-sys`), which holds the `unsafe`
//! and issues only the calls it has checked.

use std::io;
use std::path::{Path, PathBuf};

use lemnos_driver_rk_mpp::{Block, Device as ServiceDevice};
use lemnos_driver_rk_mpp_sys::{ServiceNode, ServiceSession};

use crate::mpp::NODE;

/// The service node's path, if this kernel has one.
#[must_use]
pub fn find() -> Option<PathBuf> {
    find_at(Path::new(NODE))
}

pub(crate) fn find_at(node: &Path) -> Option<PathBuf> {
    node.exists().then(|| node.to_path_buf())
}

/// An open service file, as the [`Jpeg`](crate::Jpeg) session uses it.
pub trait Device {
    /// Binds the file to a device type (`CMD_INIT_CLIENT_TYPE`). `EINVAL` when the kernel has no
    /// such block.
    fn init_client(&mut self, device_type: u32) -> io::Result<()>;
    /// The bound block's hardware id (`CMD_QUERY_HW_ID`).
    fn hw_id(&mut self) -> io::Result<u32>;
    /// Lets go of every dma-buf the file imported (`CMD_RESET_SESSION`).
    fn release_buffers(&mut self) -> io::Result<()>;
}

/// Opens the service node at `node` for a JPEG session: a session that may bind to the JPEG
/// blocks found behind an IOMMU now.
pub fn open(node: &Path) -> io::Result<ServiceSession> {
    ServiceNode::new(node).open()
}

/// The block a device type names, if it is one of the JPEG blocks.
fn block_of(device_type: u32) -> Option<Block> {
    [Block::JpegDecoder, Block::JpegEncoder]
        .into_iter()
        .find(|block| block.facts().client == device_type)
}

impl Device for ServiceSession {
    fn init_client(&mut self, device_type: u32) -> io::Result<()> {
        let block =
            block_of(device_type).ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
        self.bind(block)
    }

    fn hw_id(&mut self) -> io::Result<u32> {
        ServiceSession::hw_id(self)
    }

    fn release_buffers(&mut self) -> io::Result<()> {
        self.reset()
    }
}

#[cfg(test)]
mod tests {
    use lemnos_driver_rk_mpp::uapi::{self, Command};

    use super::*;
    use crate::Variant;
    use crate::mpp;

    #[test]
    fn this_crates_copy_of_the_service_agrees_with_the_shared_layers() {
        assert_eq!(mpp::IOC_CFG_V1, uapi::IOC_CFG_V1);
        assert_eq!(size_of::<mpp::Msg>(), size_of::<uapi::MsgV1>());
        assert_eq!(
            size_of::<mpp::OffsetElem>(),
            size_of::<uapi::RegOffsetElem>()
        );
        let commands = [
            (mpp::CMD_QUERY_HW_SUPPORT, Command::QueryHwSupport),
            (mpp::CMD_QUERY_HW_ID, Command::QueryHwId),
            (mpp::CMD_INIT_CLIENT_TYPE, Command::InitClientType),
            (mpp::CMD_INIT_TRANS_TABLE, Command::InitTransTable),
            (mpp::CMD_SET_REG_WRITE, Command::SetRegWrite),
            (mpp::CMD_SET_REG_READ, Command::SetRegRead),
            (mpp::CMD_SET_REG_ADDR_OFFSET, Command::SetRegAddrOffset),
            (mpp::CMD_POLL_HW_FINISH, Command::PollHwFinish),
            (mpp::CMD_RESET_SESSION, Command::ResetSession),
            (mpp::CMD_RELEASE_FD, Command::ReleaseFd),
        ];
        for (number, command) in commands {
            assert_eq!(number, command.code(), "{command:?}");
        }
    }

    #[test]
    fn every_variant_binds_to_a_block_the_shared_layer_knows() {
        for (variant, block) in [
            (Variant::DecoderV1, Block::JpegDecoder),
            (Variant::DecoderV2, Block::JpegDecoder),
            (Variant::EncoderV1, Block::JpegEncoder),
            (Variant::EncoderV2, Block::JpegEncoder),
        ] {
            assert_eq!(block_of(variant.device_type()), Some(block), "{variant:?}");
        }
        // A device type of another block is refused before the session is asked anything.
        assert_eq!(block_of(Block::Vdpu2.facts().client), None);
        assert_eq!(block_of(u32::MAX), None);
    }

    #[test]
    fn a_node_that_is_not_there_is_an_error() {
        let error = open(Path::new("/nonexistent/mpp_service")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }
}
