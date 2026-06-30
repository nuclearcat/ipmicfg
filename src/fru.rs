//! Reading and parsing IPMI Platform Management FRU Information.
//!
//! FRU data is fetched with the Storage netfn "Get FRU Inventory Area Info"
//! (0x10) and "Read FRU Data" (0x11) commands, then decoded according to the
//! "Platform Management FRU Information Storage Definition".

use ipmi_rs::connection::NetFn;

use crate::conn::Conn;

const NETFN_STORAGE: u8 = 0x0A;
const CMD_GET_FRU_AREA_INFO: u8 = 0x10;
const CMD_READ_FRU_DATA: u8 = 0x11;

/// Decoded contents of a single FRU device.
#[derive(Default)]
pub struct Fru {
    pub chassis: ChassisInfo,
    pub board: BoardInfo,
    pub product: ProductInfo,
}

#[derive(Default)]
pub struct ChassisInfo {
    pub chassis_type: Option<String>,
    pub part_number: Option<String>,
    pub serial: Option<String>,
}

#[derive(Default)]
pub struct BoardInfo {
    pub mfg_date: Option<String>,
    pub manufacturer: Option<String>,
    pub product_name: Option<String>,
    pub serial: Option<String>,
    pub part_number: Option<String>,
}

#[derive(Default)]
pub struct ProductInfo {
    pub manufacturer: Option<String>,
    pub product_name: Option<String>,
    pub part_number: Option<String>,
    pub version: Option<String>,
    pub serial: Option<String>,
    pub asset_tag: Option<String>,
}

impl Fru {
    /// True if no usable string fields were decoded.
    pub fn is_empty(&self) -> bool {
        let c = &self.chassis;
        let b = &self.board;
        let p = &self.product;
        c.part_number.is_none()
            && c.serial.is_none()
            && b.manufacturer.is_none()
            && b.product_name.is_none()
            && b.serial.is_none()
            && b.part_number.is_none()
            && p.manufacturer.is_none()
            && p.product_name.is_none()
            && p.serial.is_none()
            && p.part_number.is_none()
            && p.asset_tag.is_none()
    }
}

/// Read and parse the FRU at the given device id (0 is the primary FRU).
pub fn read(conn: &mut Conn, fru_id: u8) -> Result<Fru, String> {
    let size = area_size(conn, fru_id)?;
    if size == 0 {
        return Err("FRU area reports zero size".to_string());
    }
    let raw = read_all(conn, fru_id, size)?;
    Ok(parse(&raw))
}

/// Get the size of a FRU inventory area, in bytes.
fn area_size(conn: &mut Conn, fru_id: u8) -> Result<usize, String> {
    let resp = conn
        .send_raw(NetFn::from(NETFN_STORAGE), CMD_GET_FRU_AREA_INFO, vec![fru_id])
        .map_err(|e| format!("Get FRU Area Info failed: {e}"))?;
    if resp.cc() != 0 {
        return Err(format!("Get FRU Area Info: completion code 0x{:02X}", resp.cc()));
    }
    let data = resp.data();
    if data.len() < 2 {
        return Err("Get FRU Area Info: short response".to_string());
    }
    Ok(u16::from_le_bytes([data[0], data[1]]) as usize)
}

/// Read the whole FRU area in chunks.
fn read_all(conn: &mut Conn, fru_id: u8, size: usize) -> Result<Vec<u8>, String> {
    const CHUNK: usize = 16;
    let mut out = Vec::with_capacity(size);
    let mut offset = 0usize;

    while offset < size {
        let want = CHUNK.min(size - offset) as u8;
        let off = offset as u16;
        let req = vec![fru_id, (off & 0xFF) as u8, (off >> 8) as u8, want];
        let resp = conn
            .send_raw(NetFn::from(NETFN_STORAGE), CMD_READ_FRU_DATA, req)
            .map_err(|e| format!("Read FRU Data failed at offset {offset}: {e}"))?;
        if resp.cc() != 0 {
            return Err(format!(
                "Read FRU Data at offset {offset}: completion code 0x{:02X}",
                resp.cc()
            ));
        }
        let data = resp.data();
        if data.is_empty() {
            break;
        }
        let returned = data[0] as usize;
        let bytes = &data[1..];
        let take = returned.min(bytes.len());
        if take == 0 {
            break;
        }
        out.extend_from_slice(&bytes[..take]);
        offset += take;
    }

    Ok(out)
}

/// Parse a complete FRU image into structured fields.
fn parse(data: &[u8]) -> Fru {
    let mut fru = Fru::default();
    if data.len() < 8 || data[0] != 0x01 {
        return fru;
    }

    let area_offset = |idx: usize| -> Option<usize> {
        let v = *data.get(idx)? as usize * 8;
        if v == 0 || v >= data.len() {
            None
        } else {
            Some(v)
        }
    };

    if let Some(off) = area_offset(2) {
        fru.chassis = parse_chassis(&data[off..]);
    }
    if let Some(off) = area_offset(3) {
        fru.board = parse_board(&data[off..]);
    }
    if let Some(off) = area_offset(4) {
        fru.product = parse_product(&data[off..]);
    }

    fru
}

fn parse_chassis(area: &[u8]) -> ChassisInfo {
    let mut info = ChassisInfo::default();
    if area.len() < 3 {
        return info;
    }
    let code = area[2];
    let mut ty = chassis_type_name(code)
        .map(String::from)
        .unwrap_or_else(|| format!("Unknown (0x{code:02X})"));
    // 0x01 "Other" and 0x02 "Unknown" are the SMBIOS placeholders a vendor
    // leaves when it never programs a real enclosure type.
    if matches!(code, 0x01 | 0x02) {
        ty.push_str(&crate::ui::dim(" — not set by vendor"));
    }
    info.chassis_type = Some(ty);
    let mut idx = 3;
    info.part_number = next_field(area, &mut idx);
    info.serial = next_field(area, &mut idx);
    info
}

fn parse_board(area: &[u8]) -> BoardInfo {
    let mut info = BoardInfo::default();
    if area.len() < 6 {
        return info;
    }
    let minutes = u32::from_le_bytes([area[3], area[4], area[5], 0]);
    info.mfg_date = format_mfg_date(minutes);
    let mut idx = 6;
    info.manufacturer = next_field(area, &mut idx);
    info.product_name = next_field(area, &mut idx);
    info.serial = next_field(area, &mut idx);
    info.part_number = next_field(area, &mut idx);
    info
}

fn parse_product(area: &[u8]) -> ProductInfo {
    let mut info = ProductInfo::default();
    if area.len() < 3 {
        return info;
    }
    let mut idx = 3;
    info.manufacturer = next_field(area, &mut idx);
    info.product_name = next_field(area, &mut idx);
    info.part_number = next_field(area, &mut idx);
    info.version = next_field(area, &mut idx);
    info.serial = next_field(area, &mut idx);
    info.asset_tag = next_field(area, &mut idx);
    info
}

/// Decode one type/length field starting at `*idx`, advancing `*idx` past it.
/// Returns `None` at the end-of-fields marker (0xC1) or when out of data.
fn next_field(area: &[u8], idx: &mut usize) -> Option<String> {
    let type_length = *area.get(*idx)?;
    if type_length == 0xC1 {
        return None;
    }
    let ty = (type_length >> 6) & 0x03;
    let len = (type_length & 0x3F) as usize;
    let start = *idx + 1;
    let end = start + len;
    let bytes = area.get(start..end)?;
    *idx = end;

    if len == 0 {
        return Some(String::new());
    }

    let decoded = match ty {
        0b11 => bytes.iter().map(|&b| b as char).collect::<String>(), // 8-bit ASCII / Latin-1
        0b10 => decode_6bit_ascii(bytes),
        0b01 => decode_bcd_plus(bytes),
        _ => bytes
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" "),
    };

    let trimmed = decoded.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Decode 6-bit packed ASCII (4 characters per 3 bytes).
fn decode_6bit_ascii(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let packed = b0 | (b1 << 8) | (b2 << 16);
        let chars = match chunk.len() {
            1 => 1,
            2 => 2,
            _ => 4,
        };
        for i in 0..chars {
            let six = (packed >> (6 * i)) & 0x3F;
            out.push((0x20 + six as u8) as char);
        }
    }
    out
}

/// Decode BCD-plus encoding.
fn decode_bcd_plus(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        for nibble in [b >> 4, b & 0x0F] {
            let c = match nibble {
                0x0..=0x9 => (b'0' + nibble) as char,
                0xA => ' ',
                0xB => '-',
                0xC => '.',
                _ => continue,
            };
            out.push(c);
        }
    }
    out
}

/// FRU board manufacturing dates are minutes since 1996-01-01 00:00 UTC.
fn format_mfg_date(minutes: u32) -> Option<String> {
    if minutes == 0 {
        return None;
    }
    // Days since 1996-01-01, plus leftover minutes.
    let total_minutes = minutes as i64;
    let days = total_minutes / (24 * 60);
    let min_of_day = total_minutes % (24 * 60);
    let hour = min_of_day / 60;
    let minute = min_of_day % 60;

    // 1996-01-01 is 9497 days after the 1970-01-01 unix epoch.
    let epoch_days = days + 9497;
    let (y, m, d) = civil_from_days(epoch_days);
    Some(format!("{y:04}-{m:02}-{d:02} {hour:02}:{minute:02}"))
}

/// Convert days-since-unix-epoch into a (year, month, day) civil date.
/// Algorithm from Howard Hinnant's `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// SMBIOS / FRU chassis type codes (subset of the common values).
///
/// Returns `None` for codes without a friendly name (including the SMBIOS
/// "Unknown" code 0x02); the caller renders those as `Unknown (0xNN)` so the
/// raw byte stays visible.
fn chassis_type_name(code: u8) -> Option<&'static str> {
    Some(match code {
        0x01 => "Other",
        0x03 => "Desktop",
        0x04 => "Low Profile Desktop",
        0x05 => "Pizza Box",
        0x06 => "Mini Tower",
        0x07 => "Tower",
        0x08 => "Portable",
        0x09 => "Laptop",
        0x0A => "Notebook",
        0x0D => "All in One",
        0x11 => "Main Server Chassis",
        0x12 => "Expansion Chassis",
        0x17 => "Rack Mount Chassis",
        0x1C => "Blade",
        0x1D => "Blade Enclosure",
        _ => return None,
    })
}
