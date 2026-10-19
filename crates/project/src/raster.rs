//! Binary codec for `TiledImage`s inside project files.
//!
//! Layout (little endian): magic `SCTILE`, u16 version, u32 width, u32 height, u8 comps,
//! 4 × f32 fill, u32 allocated-tile count, then per tile: u32 index + `TILE_TEXELS × comps` f16.
//! Only allocated tiles are stored (sparse layers stay small); the zip layer compresses them.

use std::sync::Arc;

use half::f16;
use subcraft_image::{TILE_SIZE, TILE_TEXELS, Tile, TileSlot, TiledImage};

const MAGIC: &[u8; 6] = b"SCTILE";
const VERSION: u16 = 1;
/// Maximum resident allocation admitted for a single decoded sparse raster.
pub const MAX_DECODED_RASTER_BYTES: u64 = 512 * 1024 * 1024;
const HEADER_BYTES: usize = 37;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RasterError {
    #[error("not a SubCraft raster blob")]
    BadMagic,
    #[error("unsupported raster blob version {0}")]
    Version(u16),
    #[error("truncated raster blob")]
    Truncated,
    #[error("invalid raster: {0}")]
    Invalid(String),
}

pub fn encode(img: &TiledImage) -> Vec<u8> {
    let comps = img.comps();
    let allocated: Vec<usize> = (0..img.tile_count())
        .filter(|&i| img.is_allocated(i))
        .collect();
    let mut out = Vec::with_capacity(40 + allocated.len() * (4 + TILE_TEXELS * comps * 2));
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&img.width().to_le_bytes());
    out.extend_from_slice(&img.height().to_le_bytes());
    out.push(comps as u8);
    for f in img.fill() {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out.extend_from_slice(&(allocated.len() as u32).to_le_bytes());
    for i in allocated {
        out.extend_from_slice(&(i as u32).to_le_bytes());
        let tile = img.tile(i).expect("allocated");
        out.extend_from_slice(bytemuck::cast_slice(&tile.data));
    }
    out
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], RasterError> {
        let end = self.at.checked_add(n).ok_or(RasterError::Truncated)?;
        let s = self.b.get(self.at..end).ok_or(RasterError::Truncated)?;
        self.at = end;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32, RasterError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }
    fn f32(&mut self) -> Result<f32, RasterError> {
        Ok(f32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }
}

struct Layout {
    width: u32,
    height: u32,
    comps: usize,
    fill: [f32; 4],
    allocated: usize,
    tile_bytes: usize,
    decoded_bytes: u64,
}

fn invalid(message: &str) -> RasterError {
    RasterError::Invalid(message.into())
}

// Validate byte counts before allocating the tile table or any tile payload.
// This is deliberately independent of TiledImage's infallible constructor.
fn preflight(bytes: &[u8]) -> Result<Layout, RasterError> {
    let mut r = Reader { b: bytes, at: 0 };
    if r.take(6)? != MAGIC {
        return Err(RasterError::BadMagic);
    }
    let version = u16::from_le_bytes(r.take(2)?.try_into().expect("2 bytes"));
    if version != VERSION {
        return Err(RasterError::Version(version));
    }
    let (width, height) = (r.u32()?, r.u32()?);
    let comps = r.take(1)?[0] as usize;
    if !(1..=4).contains(&comps) || width == 0 || height == 0 || width > 16384 || height > 16384 {
        return Err(RasterError::Invalid(format!("{width}x{height}x{comps}")));
    }
    let fill = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
    if fill.iter().any(|v| !v.is_finite()) {
        return Err(invalid("non-finite fill"));
    }
    // An unallocated slot may later materialize as f16. Do not admit a fill
    // that would silently become infinity when painted or composited.
    if fill[..comps].iter().any(|v| !f16::from_f32(*v).is_finite()) {
        return Err(invalid("fill exceeds finite half-float range"));
    }
    let allocated = r.u32()? as usize;
    let tile_count = (width.div_ceil(TILE_SIZE) as usize)
        .checked_mul(height.div_ceil(TILE_SIZE) as usize)
        .ok_or_else(|| invalid("tile grid overflow"))?;
    if allocated > tile_count {
        return Err(invalid("allocated tile count exceeds tile grid"));
    }
    let tile_bytes = TILE_TEXELS
        .checked_mul(comps)
        .and_then(|n| n.checked_mul(2))
        .ok_or_else(|| invalid("tile size overflow"))?;
    let expected = tile_bytes
        .checked_add(4)
        .and_then(|n| n.checked_mul(allocated))
        .and_then(|n| n.checked_add(HEADER_BYTES))
        .ok_or_else(|| invalid("raster size overflow"))?;
    if bytes.len() < expected {
        return Err(RasterError::Truncated);
    }
    if bytes.len() != expected {
        return Err(invalid("trailing raster bytes"));
    }
    let decoded_bytes = allocation_cost(tile_count, allocated, tile_bytes)?;
    // At most 65,536 entries after dimension validation, and only allocated
    // after proving the exact payload length and resident allocation budget.
    let mut seen = vec![false; tile_count];
    for _ in 0..allocated {
        let i = r.u32()? as usize;
        if i >= tile_count {
            return Err(invalid("tile index outside grid"));
        }
        if seen[i] {
            return Err(invalid("duplicate tile index"));
        }
        seen[i] = true;
        if r.take(tile_bytes)?
            .chunks_exact(2)
            .any(|v| !f16::from_le_bytes([v[0], v[1]]).is_finite())
        {
            return Err(invalid("non-finite tile texel"));
        }
    }
    Ok(Layout {
        width,
        height,
        comps,
        fill,
        allocated,
        tile_bytes,
        decoded_bytes,
    })
}

fn allocation_cost(
    tile_count: usize,
    allocated: usize,
    tile_bytes: usize,
) -> Result<u64, RasterError> {
    // Account for Arc strong/weak counters, boxed slice and image/table objects,
    // in addition to texel storage. Allocator-private bookkeeping is excluded.
    let per_tile = (tile_bytes as u64)
        .checked_add(std::mem::size_of::<Tile>() as u64 + 2 * std::mem::size_of::<usize>() as u64)
        .ok_or_else(|| invalid("decoded tile size overflow"))?;
    let cost = (tile_count as u64)
        .checked_mul(std::mem::size_of::<TileSlot>() as u64)
        .and_then(|n| n.checked_add((allocated as u64).checked_mul(per_tile)?))
        .and_then(|n| {
            n.checked_add(
                std::mem::size_of::<TiledImage>() as u64
                    + std::mem::size_of::<Vec<TileSlot>>() as u64
                    + 2 * std::mem::size_of::<usize>() as u64,
            )
        })
        .ok_or_else(|| invalid("decoded raster size overflow"))?;
    if cost > MAX_DECODED_RASTER_BYTES {
        return Err(invalid("decoded raster exceeds 512 MiB limit"));
    }
    Ok(cost)
}

/// Validate the complete sparse raster and report its decoded resident cost.
/// Archive readers can sum this before admitting multiple rasters. This
/// includes sparse slot metadata; it does not charge unallocated texel data.
pub fn decoded_cost(bytes: &[u8]) -> Result<u64, RasterError> {
    Ok(preflight(bytes)?.decoded_bytes)
}

pub fn decode(bytes: &[u8]) -> Result<TiledImage, RasterError> {
    let layout = preflight(bytes)?;
    let mut img = TiledImage::new(layout.width, layout.height, layout.comps, layout.fill);
    let mut r = Reader {
        b: bytes,
        at: HEADER_BYTES,
    };
    for _ in 0..layout.allocated {
        let i = r.u32()? as usize;
        let raw = r.take(layout.tile_bytes)?;
        let data: Vec<f16> = raw
            .chunks_exact(2)
            .map(|c| f16::from_le_bytes([c[0], c[1]]))
            .collect();
        img.set_slot(
            i,
            Some(Arc::new(Tile {
                data: data.into_boxed_slice(),
            })),
        );
    }
    Ok(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allocated_blob() -> Vec<u8> {
        let mut img = TiledImage::new(128, 64, 1, [0.0; 4]);
        img.set(1, 1, [1.0; 4]);
        img.set(65, 1, [0.5; 4]);
        encode(&img)
    }

    #[test]
    fn rejects_duplicate_indices_trailing_bytes_and_impossible_counts() {
        let bytes = allocated_blob();
        let mut duplicate = bytes.clone();
        let second = HEADER_BYTES + 4 + TILE_TEXELS * 2;
        duplicate[second..second + 4].copy_from_slice(&0_u32.to_le_bytes());
        assert!(matches!(decode(&duplicate), Err(RasterError::Invalid(_))));
        assert!(decoded_cost(&duplicate).is_err());
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(matches!(decode(&trailing), Err(RasterError::Invalid(_))));
        let mut count = bytes.clone();
        count[33..37].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(decode(&count), Err(RasterError::Invalid(_))));
        let mut index = bytes;
        index[HEADER_BYTES..HEADER_BYTES + 4].copy_from_slice(&2_u32.to_le_bytes());
        assert!(matches!(decode(&index), Err(RasterError::Invalid(_))));
    }

    #[test]
    fn rejects_nonfinite_fills_and_half_float_texels() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, f32::MAX] {
            let mut bytes = allocated_blob();
            bytes[17..21].copy_from_slice(&bad.to_le_bytes());
            assert!(matches!(decode(&bytes), Err(RasterError::Invalid(_))));
        }
        for bad in [f16::NAN, f16::INFINITY, f16::NEG_INFINITY] {
            let mut bytes = allocated_blob();
            bytes[HEADER_BYTES + 4..HEADER_BYTES + 6].copy_from_slice(&bad.to_le_bytes());
            assert!(matches!(decode(&bytes), Err(RasterError::Invalid(_))));
        }
        let mut bytes = allocated_blob();
        bytes[17..21].copy_from_slice(&65504.0_f32.to_le_bytes());
        assert!(decode(&bytes).is_ok());
    }

    #[test]
    fn sparse_maximum_grid_is_valid_and_costs_metadata_not_dense_pixels() {
        let img = TiledImage::new(16384, 16384, 4, [0.0; 4]);
        let bytes = encode(&img);
        assert_eq!(bytes.len(), HEADER_BYTES);
        let cost = decoded_cost(&bytes).unwrap();
        assert!(cost >= 65536 * std::mem::size_of::<TileSlot>() as u64);
        assert!(cost < 1024 * 1024);
        assert_eq!(decode(&bytes).unwrap(), img);
        let mut oversized = bytes;
        oversized[8..12].copy_from_slice(&16385_u32.to_le_bytes());
        assert!(decode(&oversized).is_err());
    }

    #[test]
    fn allocation_budget_boundary_is_checked_without_allocating_payloads() {
        let slots = 65536;
        let tile_bytes = TILE_TEXELS * 4 * 2;
        let per_tile =
            (tile_bytes + std::mem::size_of::<Tile>() + 2 * std::mem::size_of::<usize>()) as u64;
        let base = allocation_cost(slots, 0, tile_bytes).unwrap();
        let fitting = ((MAX_DECODED_RASTER_BYTES - base) / per_tile) as usize;
        assert!(allocation_cost(slots, fitting, tile_bytes).unwrap() <= MAX_DECODED_RASTER_BYTES);
        assert!(allocation_cost(slots, fitting + 1, tile_bytes).is_err());
        assert!(allocation_cost(usize::MAX, usize::MAX, usize::MAX).is_err());
    }

    #[test]
    fn checked_reader_and_every_truncation_fail_without_panicking() {
        let mut r = Reader {
            b: &[],
            at: usize::MAX,
        };
        assert_eq!(r.take(1), Err(RasterError::Truncated));
        let bytes = allocated_blob();
        for end in 0..bytes.len() {
            assert!(decode(&bytes[..end]).is_err(), "prefix {end}");
        }
        assert!(decoded_cost(&bytes).unwrap() > (2 * TILE_TEXELS * 2) as u64);
    }

    #[test]
    fn roundtrip_sparse_exact() {
        let mut img = TiledImage::new(200, 130, 2, [0.25, 0.0, 0.0, 0.0]);
        img.set(5, 5, [1.0, 0.5, 0.0, 0.0]);
        img.set(150, 100, [0.125, 1.0, 0.0, 0.0]);
        let bytes = encode(&img);
        let back = decode(&bytes).unwrap();
        assert_eq!(back, img);
        assert_eq!(back.allocated_tiles(), 2);
        assert_eq!(back.fill(), img.fill());
    }

    #[test]
    fn rejects_garbage_and_truncation() {
        assert_eq!(decode(b"nope").unwrap_err(), RasterError::Truncated);
        assert_eq!(
            decode(b"XXXXXXXXXXXXXXXXXXXX").unwrap_err(),
            RasterError::BadMagic
        );
        let mut img = TiledImage::new(64, 64, 1, [0.0; 4]);
        img.set(1, 1, [1.0; 4]);
        let bytes = encode(&img);
        assert_eq!(
            decode(&bytes[..bytes.len() - 3]).unwrap_err(),
            RasterError::Truncated
        );
    }
}
