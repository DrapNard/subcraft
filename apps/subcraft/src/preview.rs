//! Real offscreen GPU preview command for the desktop binary.

use std::path::Path;

use glam::{Mat4, Vec3};
use subcraft_render::{SHADED_CHANNELS, ViewParams, ViewportRenderer};

const DEFAULT_SIZE: [u32; 2] = [1024, 1024];
const MAX_DIMENSION: u32 = 4096;
const MAX_PIXELS: u64 = 8 * 1024 * 1024;

pub fn run(args: &[String]) -> anyhow::Result<()> {
    if args.len() != 3 && args.len() != 5 {
        anyhow::bail!(
            "preview expects a project, PNG path, and optionally width and height\nusage: subcraft preview <project.subcraft> <preview.png> [width height]"
        );
    }
    let output = Path::new(&args[2]);
    if !output
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
    {
        anyhow::bail!("preview output must have a .png extension");
    }
    let size = if args.len() == 5 {
        [
            parse_dimension(&args[3], "width")?,
            parse_dimension(&args[4], "height")?,
        ]
    } else {
        DEFAULT_SIZE
    };
    validate_size(size)?;

    let mut session = subcraft_app::Session::open_project(Path::new(&args[1]))
        .map_err(|e| anyhow::anyhow!("{}: {e}", args[1]))?;
    session.update_composites();

    let Some((adapter, device, queue)) = subcraft_render::headless::device() else {
        anyhow::bail!("no usable wgpu adapter is available for offscreen preview");
    };
    let limits = device.limits();
    if size[0] > limits.max_texture_dimension_2d || size[1] > limits.max_texture_dimension_2d {
        anyhow::bail!(
            "requested preview size exceeds adapter texture limit {}",
            limits.max_texture_dimension_2d
        );
    }

    let mut renderer = ViewportRenderer::new(&device, &queue);
    let mesh = &session.scene().mesh;
    renderer.set_mesh(&device, mesh);
    for set in &session.document().texture_sets {
        if let Some(composite) = session.composite(set.id) {
            for channel in SHADED_CHANNELS {
                if let Some(image) = composite.channels.get(&channel) {
                    renderer.upload_channel(
                        &device,
                        &queue,
                        set.material_slot,
                        channel,
                        image,
                        None,
                    );
                }
            }
        }
        if let Some(maps) = session.mesh_maps(set.id) {
            for (kind, image) in maps.iter() {
                renderer.upload_mesh_map(&device, &queue, set.material_slot, kind, image);
            }
        }
    }

    let center = mesh.bounds.center();
    let radius = (mesh.bounds.diagonal() * 0.5).max(0.01);
    let direction = Vec3::new(2.1, 1.45, 2.4).normalize();
    let eye = center + direction * radius * 3.25;
    let view = Mat4::look_at_rh(eye, center, Vec3::Y);
    let aspect = size[0] as f32 / size[1] as f32;
    let near = (radius * 0.01).max(0.001);
    let far = (radius * 20.0).max(near + 1.0);
    let projection = Mat4::perspective_rh(45.0f32.to_radians(), aspect, near, far);
    let mut params = ViewParams::new(view, projection);
    params.antialias = false;
    let target = renderer.render(&device, &queue, size, &params);
    let rgba = target.read_rgba8(&device, &queue);
    let image = image::RgbaImage::from_raw(size[0], size[1], rgba)
        .ok_or_else(|| anyhow::anyhow!("renderer returned an unexpected RGBA buffer size"))?;
    image.save(output)?;
    println!(
        "Rendered {}×{} offscreen preview with {} ({:?}) to {}",
        size[0],
        size[1],
        adapter.get_info().name,
        adapter.get_info().backend,
        output.display()
    );
    Ok(())
}

fn parse_dimension(text: &str, label: &str) -> anyhow::Result<u32> {
    let value: u32 = text
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid preview {label}: {e}"))?;
    if value == 0 || value > MAX_DIMENSION {
        anyhow::bail!("preview {label} must be in 1..={MAX_DIMENSION}");
    }
    Ok(value)
}

fn validate_size(size: [u32; 2]) -> anyhow::Result<()> {
    if size.contains(&0) || size[0] > MAX_DIMENSION || size[1] > MAX_DIMENSION {
        anyhow::bail!("preview dimensions must be in 1..={MAX_DIMENSION}");
    }
    if u64::from(size[0]) * u64::from(size[1]) > MAX_PIXELS {
        anyhow::bail!("preview area is limited to {MAX_PIXELS} pixels");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_dimensions_are_nonzero_and_bounded_by_dimension_and_area() {
        assert!(validate_size([1024, 1024]).is_ok());
        assert!(validate_size([0, 100]).is_err());
        assert!(validate_size([MAX_DIMENSION + 1, 1]).is_err());
        assert!(validate_size([MAX_DIMENSION, MAX_DIMENSION]).is_err());
        assert!(validate_size([4096, 2048]).is_ok());
    }
}
