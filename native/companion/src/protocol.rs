use std::io::{self, Read, Write};

use serde_json::Value;

pub const MAX_PHYSICAL_CHUNK: usize = 1024 * 1024;
const LENGTH_MASK: u32 = 0x3fff_ffff;

/// Read one framed record. `None` is used only for the initial record, before
/// the host's protocol version has been negotiated; both supported versions
/// use the legacy single-chunk encoding for `init`.
pub fn read_json_record<R: Read>(
    reader: &mut R,
    version: Option<u32>,
) -> io::Result<Option<Value>> {
    let Some(header) = read_header(reader)? else {
        return Ok(None);
    };
    let version = version.unwrap_or(4);

    let mut record = Vec::new();
    if version == 4 {
        let length = header as usize;
        read_chunk(reader, length, &mut record)?;
    } else if version == 5 {
        let mut kind = header >> 30;
        let mut length = (header & LENGTH_MASK) as usize;
        let mut started = false;
        loop {
            match kind {
                0 if !started => {
                    append_chunk(reader, length, &mut record)?;
                    break;
                }
                0 => return Err(invalid_data("single chunk interrupted a record")),
                1 if !started => {
                    append_chunk(reader, length, &mut record)?;
                    started = true;
                    let next = read_header(reader)?
                        .ok_or_else(|| invalid_data("truncated chunk sequence"))?;
                    kind = next >> 30;
                    length = (next & LENGTH_MASK) as usize;
                    if kind != 2 && kind != 3 {
                        return Err(invalid_data("invalid chunk sequence after start"));
                    }
                }
                2 if started => {
                    append_chunk(reader, length, &mut record)?;
                    let next = read_header(reader)?
                        .ok_or_else(|| invalid_data("truncated chunk sequence"))?;
                    kind = next >> 30;
                    length = (next & LENGTH_MASK) as usize;
                    if kind != 2 && kind != 3 {
                        return Err(invalid_data("invalid chunk sequence continuation"));
                    }
                }
                3 if started => {
                    append_chunk(reader, length, &mut record)?;
                    break;
                }
                1 => return Err(invalid_data("nested start chunk")),
                2 | 3 => return Err(invalid_data("continuation chunk appeared without a start")),
                _ => unreachable!(),
            }
        }
    } else {
        return Err(invalid_data("unsupported protocol version"));
    }

    serde_json::from_slice(&record)
        .map(Some)
        .map_err(|error| invalid_data(format!("invalid JSON record: {error}")))
}

pub fn write_json_record<W: Write>(writer: &mut W, version: u32, value: &Value) -> io::Result<()> {
    let record = serde_json::to_vec(value)
        .map_err(|error| invalid_data(format!("could not encode JSON record: {error}")))?;
    if record.is_empty() {
        return Err(invalid_data("JSON record cannot be empty"));
    }

    match version {
        4 => {
            if record.len() > MAX_PHYSICAL_CHUNK {
                return Err(invalid_data("protocol v4 records must fit in one chunk"));
            }
            write_chunk(writer, 0, &record)?;
        }
        5 => {
            if record.len() <= MAX_PHYSICAL_CHUNK {
                write_chunk(writer, 0, &record)?;
            } else {
                let mut chunks = record.chunks(MAX_PHYSICAL_CHUNK).peekable();
                if let Some(first) = chunks.next() {
                    write_chunk(writer, 1, first)?;
                }
                while let Some(chunk) = chunks.next() {
                    let kind = if chunks.peek().is_none() { 3 } else { 2 };
                    write_chunk(writer, kind, chunk)?;
                }
            }
        }
        _ => return Err(invalid_data("unsupported protocol version")),
    }
    writer.flush()
}

fn read_header<R: Read>(reader: &mut R) -> io::Result<Option<u32>> {
    let mut bytes = [0_u8; 4];
    match reader.read(&mut bytes[..1])? {
        0 => return Ok(None),
        1 => {}
        _ => unreachable!(),
    }
    reader.read_exact(&mut bytes[1..])?;
    Ok(Some(u32::from_le_bytes(bytes)))
}

fn read_chunk<R: Read>(reader: &mut R, length: usize, record: &mut Vec<u8>) -> io::Result<()> {
    append_chunk(reader, length, record)
}

fn append_chunk<R: Read>(reader: &mut R, length: usize, record: &mut Vec<u8>) -> io::Result<()> {
    validate_chunk_length(length)?;
    let old_length = record.len();
    record
        .try_reserve(length)
        .map_err(|error| invalid_data(format!("could not reserve record memory: {error}")))?;
    record.resize(old_length + length, 0);
    reader.read_exact(&mut record[old_length..])
}

fn validate_chunk_length(length: usize) -> io::Result<()> {
    if length == 0 || length > MAX_PHYSICAL_CHUNK {
        return Err(invalid_data(
            "physical chunk length must be between 1 byte and 1 MiB",
        ));
    }
    Ok(())
}

fn write_chunk<W: Write>(writer: &mut W, kind: u32, chunk: &[u8]) -> io::Result<()> {
    validate_chunk_length(chunk.len())?;
    let header = ((kind & 0b11) << 30) | chunk.len() as u32;
    writer.write_all(&header.to_le_bytes())?;
    writer.write_all(chunk)
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v4_keeps_single_chunk_record_shape() {
        let expected = serde_json::json!({"type":"init", "v":4});
        let mut bytes = Vec::new();
        write_json_record(&mut bytes, 4, &expected).unwrap();
        assert_eq!(
            read_json_record(&mut bytes.as_slice(), Some(4)).unwrap(),
            Some(expected)
        );
    }

    #[test]
    fn v5_reads_ordered_continuation_chunks() {
        let expected = serde_json::json!({"payload":"x".repeat(MAX_PHYSICAL_CHUNK + 37)});
        let mut bytes = Vec::new();
        write_json_record(&mut bytes, 5, &expected).unwrap();
        assert_eq!(
            read_json_record(&mut bytes.as_slice(), Some(5)).unwrap(),
            Some(expected)
        );
    }

    #[test]
    fn malformed_or_oversized_chunks_are_rejected_before_allocation() {
        let header = ((1_u32 << 30) | ((MAX_PHYSICAL_CHUNK + 1) as u32)).to_le_bytes();
        assert!(read_json_record(&mut header.as_slice(), Some(5)).is_err());
    }
}
