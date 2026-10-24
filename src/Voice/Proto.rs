pub const ProtocolVersion: u8 = 1;

pub const ClientHello: u8 = 0x01;
pub const ClientStatus: u8 = 0x02;
pub const ClientAudio: u8 = 0x04;

pub const ServerWelcome: u8 = 0x81;
pub const ServerState: u8 = 0x82;
pub const ServerAudio: u8 = 0x84;
pub const ServerPos: u8 = 0x85;
pub const ServerBye: u8 = 0x8F;

pub const MaxOpusFrame: usize = 1000;

pub fn ReadU64(Bytes: &[u8], At: usize) -> Option<u64> {
    Some(u64::from_le_bytes(Bytes.get(At..At + 8)?.try_into().ok()?))
}

pub fn ReadU16(Bytes: &[u8], At: usize) -> Option<u16> {
    Some(u16::from_le_bytes(Bytes.get(At..At + 2)?.try_into().ok()?))
}
