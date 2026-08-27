//! The surface channel, sidecar side: bootstrap look-up, hello with the reply
//! right, the ring's surface rights, then frames as bytes. Wire bytes are the
//! contract's alone — this module never restates the grammar. The application
//! half here (`ChannelHost`) exists for conformance tests; the real
//! application implements its own in its service.

use std::ffi::CString;
use std::os::raw::{c_char, c_int};

use soksak_contract_surface::{channel_name, decode, encode, wire_length, Message};

#[repr(C)]
struct RawChannel {
    _opaque: [u8; 0],
}

#[repr(C)]
struct RawChannelHost {
    _opaque: [u8; 0],
}

unsafe extern "C" {
    fn soksak_channel_open(name: *const c_char) -> *mut RawChannel;
    fn soksak_channel_close(channel: *mut RawChannel);
    fn soksak_channel_send_hello(channel: *mut RawChannel, bytes: *const u8, len: u64) -> c_int;
    fn soksak_channel_send_surfaces(
        channel: *mut RawChannel,
        bytes: *const u8,
        len: u64,
        ports: *const u32,
        port_count: u32,
    ) -> c_int;
    fn soksak_channel_send_bytes(channel: *mut RawChannel, bytes: *const u8, len: u64) -> c_int;
    fn soksak_channel_recv(
        channel: *mut RawChannel,
        out: *mut u8,
        cap: u64,
        out_len: *mut u64,
        timeout_ms: i32,
    ) -> c_int;
    fn soksak_channel_host_check_in(name: *const c_char) -> *mut RawChannelHost;
    fn soksak_channel_host_close(host: *mut RawChannelHost);
    fn soksak_channel_host_recv(
        host: *mut RawChannelHost,
        out: *mut u8,
        cap: u64,
        out_len: *mut u64,
        ports: *mut u32,
        port_count: *mut u32,
        timeout_ms: i32,
    ) -> c_int;
    fn soksak_channel_host_send(
        host: *mut RawChannelHost,
        port: u32,
        bytes: *const u8,
        len: u64,
    ) -> c_int;
}

const RECV_CAP: usize = 4096;

fn name_for(identifier: &str) -> Result<CString, String> {
    let name = channel_name(identifier)?;
    CString::new(name).map_err(|_| "CHANNEL_NAME_INVALID".to_string())
}

pub struct SurfaceChannel {
    raw: *mut RawChannel,
}

// mach_msg is a kernel trap and safe to call concurrently on the same rights:
// every render thread sends, and only the one reader thread ever receives on
// the reply right.
unsafe impl Send for SurfaceChannel {}
unsafe impl Sync for SurfaceChannel {}

impl SurfaceChannel {
    /// Look the application's checked-in name up. The name derives from the
    /// identity — there is no handshake channel to negotiate one.
    pub fn open(identifier: &str) -> Result<Self, String> {
        let name = name_for(identifier)?;
        let raw = unsafe { soksak_channel_open(name.as_ptr()) };
        if raw.is_null() {
            return Err(format!(
                "CHANNEL_UNAVAILABLE: nothing checked in as {:?}",
                name
            ));
        }
        Ok(Self { raw })
    }

    /// Ship one message. A hello rides with the reply right, a ring moves its
    /// surface rights, everything else is bytes.
    pub fn send(&self, message: &Message, surface_ports: &[u32]) -> Result<(), String> {
        let bytes = encode(message)?;
        let code = match message {
            Message::Hello { .. } => unsafe {
                soksak_channel_send_hello(self.raw, bytes.as_ptr(), bytes.len() as u64)
            },
            Message::Ring { .. } => {
                if surface_ports.len() != message.port_count() {
                    return Err(format!(
                        "RING_PORTS_MISMATCH: {} rights for a ring of {}",
                        surface_ports.len(),
                        message.port_count()
                    ));
                }
                unsafe {
                    soksak_channel_send_surfaces(
                        self.raw,
                        bytes.as_ptr(),
                        bytes.len() as u64,
                        surface_ports.as_ptr(),
                        surface_ports.len() as u32,
                    )
                }
            }
            _ => unsafe {
                soksak_channel_send_bytes(self.raw, bytes.as_ptr(), bytes.len() as u64)
            },
        };
        if code != 0 {
            return Err(format!("CHANNEL_SEND_{code}: the message did not leave"));
        }
        Ok(())
    }

    /// One message from the application, or None on timeout.
    pub fn recv(&self, timeout_ms: i32) -> Result<Option<Message>, String> {
        let mut buffer = vec![0u8; RECV_CAP];
        let mut len: u64 = 0;
        let code = unsafe {
            soksak_channel_recv(self.raw, buffer.as_mut_ptr(), RECV_CAP as u64, &mut len, timeout_ms)
        };
        match code {
            0 => {
                let exact = wire_length(&buffer[..len as usize])?;
                decode(&buffer[..exact]).map(Some)
            }
            1 => Ok(None),
            _ => Err(format!("CHANNEL_RECV_{code}: the reply port did not answer")),
        }
    }
}

impl Drop for SurfaceChannel {
    fn drop(&mut self) {
        unsafe { soksak_channel_close(self.raw) };
    }
}

/// The application half, for conformance tests only.
pub struct ChannelHost {
    raw: *mut RawChannelHost,
}

unsafe impl Send for ChannelHost {}

impl ChannelHost {
    pub fn check_in(identifier: &str) -> Result<Self, String> {
        let name = name_for(identifier)?;
        let raw = unsafe { soksak_channel_host_check_in(name.as_ptr()) };
        if raw.is_null() {
            return Err("CHANNEL_CHECK_IN_REFUSED: the bootstrap name is taken or barred".to_string());
        }
        Ok(Self { raw })
    }

    pub fn recv(&self, timeout_ms: i32) -> Result<Option<(Message, Vec<u32>)>, String> {
        let mut buffer = vec![0u8; RECV_CAP];
        let mut len: u64 = 0;
        let mut ports = [0u32; 4];
        let mut port_count: u32 = 0;
        let code = unsafe {
            soksak_channel_host_recv(
                self.raw,
                buffer.as_mut_ptr(),
                RECV_CAP as u64,
                &mut len,
                ports.as_mut_ptr(),
                &mut port_count,
                timeout_ms,
            )
        };
        match code {
            0 => {
                let exact = wire_length(&buffer[..len as usize])?;
                let message = decode(&buffer[..exact])?;
                Ok(Some((message, ports[..port_count as usize].to_vec())))
            }
            1 => Ok(None),
            _ => Err(format!("CHANNEL_HOST_RECV_{code}: the service port did not answer")),
        }
    }

    pub fn send_to(&self, port: u32, message: &Message) -> Result<(), String> {
        let bytes = encode(message)?;
        let code = unsafe {
            soksak_channel_host_send(self.raw, port, bytes.as_ptr(), bytes.len() as u64)
        };
        if code != 0 {
            return Err(format!("CHANNEL_HOST_SEND_{code}: the reply did not leave"));
        }
        Ok(())
    }
}

impl Drop for ChannelHost {
    fn drop(&mut self) {
        unsafe { soksak_channel_host_close(self.raw) };
    }
}
