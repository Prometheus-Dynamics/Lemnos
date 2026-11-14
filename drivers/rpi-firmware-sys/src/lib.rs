//! The Raspberry Pi firmware's property mailbox, on `/dev/vcio`, for the boot count and nothing
//! else: the one place the `rpi-tryboot` fallback touches the firmware.
//!
//! The firmware counts boots in an 8-bit register (`boot_count`, wrapping at 256) that is cleared
//! when power is removed. The OS reads it at `/proc/device-tree/chosen/bootloader/count`; this
//! crate reads it and clears it through the property mailbox, the same messages `vcmailbox` sends:
//!
//! - get: tag `0x0003008d`, `vcmailbox 0x0003008d 4 4 0` (`conditional.adoc`, `boot_count`);
//! - clear: tag `0x0003808d` with the value zero, `vcmailbox 0x0003808d 4 4 0`, the same
//!   documentation's "Clear boot_count by setting it to zero".
//!
//! The message is one 28-byte buffer: size, request code `0`, the tag, the value's size (4), a
//! request code, the value, and the end tag `0`. The reply repeats it with the code `0x80000000`
//! set on success and `0x80000000 | 4` on the tag. The ioctl is `_IOWR(100, 0, char *)`, from the
//! kernel's `drivers/firmware/raspberrypi.c` (`IOCTL_MBOX_PROPERTY`); the argument is the buffer
//! itself. The device path is given by the caller, so the tests use an ordinary file.
//!
//! One more tag is sent, once, by [`Mailbox::request_rpiboot`]: the firmware's set_reboot_order
//! (`0x0003808b`, value 3 = RPIBOOT), `vcmailbox 0x0003808b 4 4 0x3`, as the dev image's own
//! `usb-boot` script sends it. It applies to the NEXT BOOT ONLY and clears itself; the bootloader's
//! stored `BOOT_ORDER` is never changed. BCM2712 boards (the Pi 5 class) only. RPIBOOT has no timeout:
//! the board then waits for a USB host until it is flashed or power-cycled. Nothing else here
//! changes the firmware's configuration, and nothing reads the device-tree files (those are the
//! caller's, see `lemnos-driver-system-slots` and `lemnos-slot-init`).

#![cfg(target_os = "linux")]

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

use lemnos_linux_sys::iowr;

/// The mailbox's ioctl: `_IOWR(100, 0, char *)`.
pub const IOCTL_MBOX_PROPERTY: u32 = iowr::<*mut u8>(b'd', 0);
/// The tag that reads the boot count.
pub const TAG_GET_BOOT_COUNT: u32 = 0x0003_008d;
/// The tag that sets the boot count (the clear sends the value zero).
pub const TAG_SET_BOOT_COUNT: u32 = 0x0003_808d;
/// The tag that sets the reboot order for the next boot only.
pub const TAG_SET_REBOOT_ORDER: u32 = 0x0003_808b;
/// The reboot order value for RPIBOOT.
pub const REBOOT_ORDER_RPIBOOT: u32 = 0x3;
/// The default device node.
pub const DEFAULT_DEVICE: &str = "/dev/vcio";

/// The message length in bytes: seven words.
pub const MESSAGE_BYTES: usize = 28;
/// The request code, and the reply code's success bit.
const REQUEST: u32 = 0;
const RESPONSE_OK: u32 = 0x8000_0000;
/// The end tag.
const END: u32 = 0;

/// The message as the mailbox takes it: seven words, in a buffer aligned for the kernel's copy.
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message(pub [u32; 8]);

impl Message {
    /// A request for `tag` with a 4-byte value. `value` is sent for a set and zero for a get.
    pub fn request(tag: u32, value: u32) -> Self {
        Self([
            MESSAGE_BYTES as u32,
            REQUEST,
            tag,
            4,
            REQUEST,
            value,
            END,
            0,
        ])
    }

    /// Whether the firmware's answer says it took the request (word 1).
    pub fn took_it(&self) -> bool {
        self.0[1] == RESPONSE_OK
    }

    /// The 4-byte value of a reply to `tag`. `None` when the reply's size, code, tag or end tag is
    /// not the one the request asked for.
    pub fn reply_value(&self, tag: u32) -> Option<u32> {
        let w = &self.0;
        let ok = w[0] == MESSAGE_BYTES as u32
            && w[1] == RESPONSE_OK
            && w[2] == tag
            && w[3] == 4
            && w[4] == (RESPONSE_OK | 4)
            && w[6] == END;
        ok.then_some(w[5])
    }
}

/// An open mailbox.
#[derive(Debug)]
pub struct Mailbox {
    file: File,
}

impl Mailbox {
    /// Opens the mailbox device at `path`. The device is only opened, nothing is sent.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self { file })
    }

    /// The boot count the firmware holds, `0` to `255`.
    pub fn boot_count(&self) -> io::Result<u8> {
        let value = self.exchange(TAG_GET_BOOT_COUNT, 0)?;
        u8::try_from(value).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))
    }

    /// Clears the boot count to zero.
    pub fn clear_boot_count(&self) -> io::Result<()> {
        self.exchange(TAG_SET_BOOT_COUNT, 0).map(drop)
    }

    /// Asks the firmware to boot into RPIBOOT at the NEXT boot only. The caller restarts. The reply's
    /// second word is checked: it is `0x80000000` when the firmware took the request, and anything
    /// else is an error. To confirm on a board: the request itself, and that `/dev/vcio` exists in
    /// the initramfs when it is made (`testing/board-runs/I5i6.md`).
    pub fn request_rpiboot(&self) -> io::Result<()> {
        let message = self.send(Message::request(TAG_SET_REBOOT_ORDER, REBOOT_ORDER_RPIBOOT))?;
        if message.took_it() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "the firmware did not take the reboot order: answer word 0x{:08x}",
                message.0[1]
            )))
        }
    }

    /// One request and its reply, through the ioctl.
    fn exchange(&self, tag: u32, value: u32) -> io::Result<u32> {
        let message = self.send(Message::request(tag, value))?;
        message
            .reply_value(tag)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "mailbox reply malformed"))
    }

    /// Sends `message` and returns the buffer as the firmware answered it.
    fn send(&self, mut message: Message) -> io::Result<Message> {
        // SAFETY: `IOCTL_MBOX_PROPERTY` takes the message buffer itself. `message` is a live,
        // exclusive, 16-byte-aligned buffer of eight words, of which the kernel reads and writes
        // the first `MESSAGE_BYTES` bytes, so the kernel stays inside it.
        let rc = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                IOCTL_MBOX_PROPERTY as _,
                message.0.as_mut_ptr(),
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tags are the firmware's: `boot_count` read and set, from `conditional.adoc`.
    #[test]
    fn the_tags_are_the_documented_ones() {
        assert_eq!(TAG_GET_BOOT_COUNT, 0x0003_008d);
        assert_eq!(TAG_SET_BOOT_COUNT, 0x0003_808d);
        assert_eq!(TAG_SET_BOOT_COUNT, TAG_GET_BOOT_COUNT | 0x8000);
    }

    /// The ioctl is `_IOWR(100, 0, char *)`: the pointer's size is the LP64 or 32-bit one.
    #[test]
    fn the_ioctl_is_the_kernels_number() {
        #[cfg(target_pointer_width = "64")]
        assert_eq!(IOCTL_MBOX_PROPERTY, 0xC008_6400);
        #[cfg(target_pointer_width = "32")]
        assert_eq!(IOCTL_MBOX_PROPERTY, 0xC004_6400);
    }

    /// The RPIBOOT request is `vcmailbox 0x0003808b 4 4 0x3`, and only the OK word means taken.
    #[test]
    fn the_rpiboot_request_is_the_dev_images_and_only_the_ok_word_means_taken() {
        let mut message = Message::request(TAG_SET_REBOOT_ORDER, REBOOT_ORDER_RPIBOOT);
        assert_eq!(message.0, [28, 0, 0x0003_808b, 4, 0, 3, 0, 0]);
        assert!(!message.took_it());
        message.0[1] = RESPONSE_OK;
        assert!(message.took_it());
        message.0[1] = RESPONSE_OK | 1;
        assert!(!message.took_it());
    }

    /// The get message is the documentation's: `0x1c 0 0x0003008d 4 0 0 0`.
    #[test]
    fn the_get_message_has_the_documented_layout() {
        let m = Message::request(TAG_GET_BOOT_COUNT, 0);
        assert_eq!(&m.0[..7], &[28, 0, 0x0003_008d, 4, 0, 0, 0]);
    }

    /// The clear message carries the value zero, as `vcmailbox 0x0003808d 4 4 0` does.
    #[test]
    fn the_clear_message_sends_zero() {
        let m = Message::request(TAG_SET_BOOT_COUNT, 0);
        assert_eq!(&m.0[..7], &[28, 0, 0x0003_808d, 4, 0, 0, 0]);
    }

    /// A documented reply decodes to its value: `boot_count` 3 in the documentation's example.
    #[test]
    fn a_documented_reply_decodes_to_the_count() {
        let reply = Message([28, 0x8000_0000, 0x0003_008d, 4, 0x8000_0004, 3, 0, 0]);
        assert_eq!(reply.reply_value(TAG_GET_BOOT_COUNT), Some(3));
    }

    /// A reply with the wrong tag, a failed code, or no end tag is malformed.
    #[test]
    fn a_malformed_reply_is_refused() {
        let good = [28, 0x8000_0000, 0x0003_008d, 4, 0x8000_0004, 3, 0, 0];
        for (index, bad) in [(1, 0), (2, 0x0003_808d), (4, 4), (6, 9)] {
            let mut words = good;
            words[index] = bad;
            assert_eq!(
                Message(words).reply_value(TAG_GET_BOOT_COUNT),
                None,
                "word {index} = {bad:#x} must be refused"
            );
        }
    }

    /// A device that is not there is an error, and an ordinary file is not a mailbox: the ioctl
    /// fails and the error is returned, not a count.
    #[test]
    fn a_missing_or_non_mailbox_device_is_an_error() {
        let dir = std::env::temp_dir().join(format!("rpi-fw-sys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("no-such-vcio");
        assert!(Mailbox::open(&missing).is_err());
        let plain = dir.join("plain");
        std::fs::write(&plain, b"not a mailbox").unwrap();
        let mailbox = Mailbox::open(&plain).unwrap();
        assert!(mailbox.boot_count().is_err());
        assert!(mailbox.clear_boot_count().is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
