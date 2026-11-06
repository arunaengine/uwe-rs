//! Decompresses Blosc 1 frames, the default compressor of Zarr v2 stores, from their public format.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::Read;

/// The flags of the frame header.
const SHUFFLE: u8 = 0x01;
const COPIED: u8 = 0x02;
const BIT_SHUFFLE: u8 = 0x04;
const UNSPLIT: u8 = 0x10;
/// Blosc splits a block into one stream per byte of an element only below these sizes.
const MAX_SPLITS: usize = 16;
const MIN_BUFFER: usize = 128;

/// The bytes that a Blosc frame holds.
pub(super) fn decompress(frame: &[u8]) -> Result<Vec<u8>, String> {
    let word = |at: usize| -> Result<usize, String> {
        let bytes = frame.get(at..at + 4).ok_or("Blosc frame is truncated")?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize)
    };
    let header = frame.get(..16).ok_or("Blosc frame is truncated")?;
    let (flags, element) = (header[2], usize::from(header[3]).max(1));
    let (total, block_size) = (word(4)?, word(8)?);
    if flags & COPIED != 0 {
        return (frame.get(16..16 + total).map(<[u8]>::to_vec))
            .ok_or("Blosc frame is truncated".into());
    }
    if flags & BIT_SHUFFLE != 0 {
        return Err("Blosc bit shuffle is not supported".into());
    }
    if block_size == 0 {
        return Ok(Vec::new());
    }
    let codec = flags >> 5;
    let blocks = total.div_ceil(block_size);
    let mut output = Vec::with_capacity(total);
    for block in 0..blocks {
        let size = block_size.min(total - block * block_size);
        let last = size < block_size;
        let mut position = word(16 + block * 4)?;
        let splits = match flags & UNSPLIT == 0
            && !last
            && element <= MAX_SPLITS
            && size / element >= MIN_BUFFER
        {
            true => element,
            false => 1,
        };
        let part = size / splits;
        let mut decoded = Vec::with_capacity(size);
        for _ in 0..splits {
            let length = word(position)?;
            position += 4;
            let input = frame
                .get(position..position + length)
                .ok_or("Blosc frame is truncated")?;
            position += length;
            match length == part {
                true => decoded.extend_from_slice(input),
                false => decoded.extend(inflate(codec, input, part)?),
            }
        }
        if flags & SHUFFLE != 0 {
            decoded = unshuffle(&decoded, element);
        }
        output.extend(decoded);
    }
    Ok(output)
}

/// One stream of a block, with the compressor that the header names.
fn inflate(codec: u8, input: &[u8], size: usize) -> Result<Vec<u8>, String> {
    let output = match codec {
        1 => lz4_flex::block::decompress(input, size).map_err(|error| error.to_string())?,
        3 => {
            let mut output = Vec::with_capacity(size);
            let mut reader = flate2::read::ZlibDecoder::new(input);
            reader
                .read_to_end(&mut output)
                .map_err(|error| error.to_string())?;
            output
        }
        4 => zstd::bulk::decompress(input, size).map_err(|error| error.to_string())?,
        0 => return Err("the Blosc compressor blosclz is not supported".into()),
        2 => return Err("the Blosc compressor snappy is not supported".into()),
        other => return Err(format!("unknown Blosc compressor {other}")),
    };
    match output.len() == size {
        true => Ok(output),
        false => Err("a Blosc stream has the wrong size".into()),
    }
}

/// Undoes the byte shuffle: the first bytes of all elements come first, then the second ones.
/// Bytes after the last whole element stay where they are.
pub(super) fn unshuffle(bytes: &[u8], element: usize) -> Vec<u8> {
    let count = bytes.len() / element;
    let mut output = bytes.to_vec();
    for index in 0..count {
        for byte in 0..element {
            output[index * element + byte] = bytes[byte * count + index];
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{decompress, unshuffle};

    #[test]
    fn reads_copied_frame() {
        let mut frame = vec![2, 1, 0x02 | 0x01, 4];
        for word in [8u32, 8, 24] {
            frame.extend(word.to_le_bytes());
        }
        frame.extend([1, 0, 0, 0, 2, 0, 0, 0]);
        assert_eq!(decompress(&frame).unwrap(), [1, 0, 0, 0, 2, 0, 0, 0]);
    }

    #[test]
    fn unshuffles_elements() {
        assert_eq!(unshuffle(&[1, 2, 0, 0, 9], 2), [1, 0, 2, 0, 9]);
    }
}
