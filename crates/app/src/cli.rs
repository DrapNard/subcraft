//! Headless SubCraft workflows. These commands use the same session, project,
//! bake and export code as the editor, without creating a window.

use std::io::Read;
use std::path::Path;

use subcraft_core::MeshMapKind;
use subcraft_export::{ExportSettings, ExportTemplate};
use subcraft_image::FileFormat;

use crate::{BakeRequest, BakeSettings, Session};

const MAX_BAKE_SIZE: u32 = 4096;
const MAX_BAKE_SAMPLES: u32 = 4096;
const MAX_BAKE_OUTPUT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PRESET_BYTES: u64 = 64 * 1024;

const HELP: &str = "SubCraft headless commands\n\
Usage:\n\
  subcraft --help | --version\n\
  subcraft inspect <project.subcraft>\n\
  subcraft validate <project.subcraft>\n\
  subcraft export <project> <output-directory> [--profile generic|godot|unity|gltf]\n\
  subcraft export-all <project> <output-directory> [--profile generic|godot|unity|gltf]\n\
  subcraft export-godot|export-all-godot <project> <output-directory>\n\
  subcraft export-unity|export-all-unity <project> <output-directory>\n\
  subcraft export-gltf|export-all-gltf <project> <output-directory>\n\
  subcraft uv-mask <project> <output-directory> [texture-set-id]\n\
  subcraft bake-mesh-maps <project> <output-directory> [--high-mesh mesh.obj|gltf|glb|fbx] [--maps normal,ao,...] [--size 1024] [--ao-samples 32]\n\
  subcraft brush-inspect <preset.scbrush>\n\
  subcraft spp-inspect <project.spp>  (SPP parsing is not implemented)\n\
  subcraft preview <project> <preview.png> [width height]\n\
Without a headless command, the graphical application starts. Projects can be
SubCraft .subcraft files. Genuine .spp open
and save are not implemented; SubCraft refuses to treat SPP as a native archive.";

/// Run a recognized headless command. Returns `false` for normal GUI launch
/// arguments, so existing desktop launch switches keep their current meaning.
pub fn run(args: &[String]) -> Result<bool, String> {
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(false);
    };
    match command {
        "--help" | "-h" | "help" => {
            println!("{HELP}");
            Ok(true)
        }
        "--version" | "-V" | "version" => {
            println!("SubCraft {} · AGPL-3.0-only", env!("CARGO_PKG_VERSION"));
            Ok(true)
        }
        "inspect" => {
            exact_count(args, 2)?;
            inspect(Path::new(&args[1]))?;
            Ok(true)
        }
        "validate" => {
            exact_count(args, 2)?;
            validate(Path::new(&args[1]))?;
            Ok(true)
        }
        "export" | "export-all" => export_command(args, None),
        "export-godot" | "export-all-godot" => export_command(args, Some("godot")),
        "export-unity" | "export-all-unity" => export_command(args, Some("unity")),
        "export-gltf" | "export-all-gltf" => export_command(args, Some("gltf")),
        "uv-mask" => uv_mask_command(args),
        "bake-mesh-maps" => bake_command(args),
        "brush-inspect" => {
            exact_count(args, 2)?;
            inspect_brush(Path::new(&args[1]))?;
            Ok(true)
        }
        "spp-inspect" => {
            if args.len() != 2 {
                return Err("spp-inspect expects one .spp path".into());
            }
            Err("SPP support is not implemented: SubCraft cannot inspect, open, edit, or save .spp projects yet".into())
        }
        // Preview needs the renderer crate and a selected GPU/software adapter. Keep
        // that dependency in the binary/UI layer rather than pulling it into app.
        "preview" => Err(
            "preview is not wired into the headless app CLI yet; use the desktop screenshot option"
                .into(),
        ),
        _ => Ok(false),
    }
}

fn exact_count(args: &[String], expected: usize) -> Result<(), String> {
    if args.len() != expected {
        Err(format!(
            "{} expects {} argument(s)\n\n{HELP}",
            args[0],
            expected.saturating_sub(1)
        ))
    } else {
        Ok(())
    }
}

fn open(path: &Path) -> Result<Session, String> {
    Session::open_project(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn validate(path: &Path) -> Result<(), String> {
    let session = open(path)?;
    let mesh = &session.scene().mesh;
    let problems = mesh.validate();
    if !problems.is_empty() {
        return Err(format!("mesh validation failed: {}", problems.join("; ")));
    }
    println!(
        "VALID: {} ({} texture set(s), {} triangles)",
        path.display(),
        session.document().texture_sets.len(),
        mesh.triangle_count()
    );
    Ok(())
}

fn inspect(path: &Path) -> Result<(), String> {
    let session = open(path)?;
    let mesh = &session.scene().mesh;
    println!("Project: {}", path.display());
    println!(
        "Mesh: {} | {} vertices | {} triangles",
        mesh.name,
        mesh.positions.len(),
        mesh.triangle_count()
    );
    println!("Texture sets: {}", session.document().texture_sets.len());
    for set in &session.document().texture_sets {
        println!(
            "  {} (id {}, material slot {}, {}×{})",
            set.name, set.id, set.material_slot, set.resolution, set.resolution
        );
        for layer in &set.layers {
            print_layer(layer, 2);
        }
    }
    Ok(())
}

fn print_layer(layer: &subcraft_doc::Layer, depth: usize) {
    let pad = "  ".repeat(depth);
    let kind = match &layer.content {
        subcraft_doc::LayerContent::Paint(_) => "Paint",
        subcraft_doc::LayerContent::Fill(_) => "Fill",
        subcraft_doc::LayerContent::Group(_) => "Group",
    };
    println!(
        "{pad}{kind}: {} (id {}, visible {}, opacity {:.2})",
        layer.name,
        layer.id,
        layer.visible,
        layer.blend.values().next().map_or(1.0, |b| b.opacity)
    );
    for child in layer.children() {
        print_layer(child, depth + 1);
    }
}

fn export_command(args: &[String], fixed_profile: Option<&str>) -> Result<bool, String> {
    if args.len() < 3 {
        return Err(format!(
            "{} expects a project and output directory\n\n{HELP}",
            args[0]
        ));
    }
    let profile_arg = if let Some(profile) = fixed_profile {
        if args.len() != 3 {
            return Err(format!("invalid {} arguments\n\n{HELP}", args[0]));
        }
        profile.to_owned()
    } else {
        match args.len() {
            3 => "generic".to_owned(),
            5 if args[3] == "--profile" => args[4].clone(),
            _ => return Err(format!("invalid {} arguments\n\n{HELP}", args[0])),
        }
    };
    let settings = export_settings(&profile_arg)?;
    let mut session = open(Path::new(&args[1]))?;
    session.update_composites();
    let all_sets = args[0].starts_with("export-all");
    let selected_id = session.selected_texture_set().id;
    let name = session.export_name();
    let export_sets: Vec<_> = session
        .document()
        .texture_sets
        .iter()
        .filter(|set| all_sets || set.id == selected_id)
        .filter_map(|set| {
            session
                .composite(set.id)
                .map(|composite| subcraft_export::ExportTextureSet {
                    name: &set.name,
                    material_slot: set.material_slot,
                    channels: &composite.channels,
                    mesh_maps: session.mesh_maps(set.id).map(|maps| maps.as_map()),
                })
        })
        .collect();
    if export_sets.is_empty() {
        return Err("selected texture set has no composite to export".into());
    }
    let plan = subcraft_export::plan(&name, &export_sets, &settings);
    let mut names = std::collections::BTreeSet::new();
    if plan
        .iter()
        .any(|(_, filename)| !names.insert(filename.clone()))
    {
        return Err(
            "export would produce duplicate filenames; rename texture sets before exporting".into(),
        );
    }
    let files = subcraft_export::export(
        &session.scene().mesh,
        &name,
        &export_sets,
        &settings,
        Path::new(&args[2]),
    )
    .map_err(|e| e.to_string())?;
    println!(
        "Exported {} map(s) from {} texture set(s) using {} profile to {}",
        files.len(),
        export_sets.len(),
        profile_arg,
        args[2]
    );
    for file in files {
        println!("  {}", file.display());
    }
    Ok(true)
}

fn export_settings(profile: &str) -> Result<ExportSettings, String> {
    let template = match profile {
        "generic" => ExportTemplate::pbr_metal_rough(),
        // Godot's StandardMaterial3D ORM texture uses R=AO, G=roughness, B=metallic.
        "godot" => {
            let mut t = ExportTemplate::gltf();
            t.name = "Godot 4 ORM".into();
            t
        }
        "unity" => ExportTemplate::unity_hdrp(),
        "gltf" => ExportTemplate::gltf(),
        other => {
            return Err(format!(
                "unknown export profile {other:?}; choose generic, godot, unity, or gltf"
            ));
        }
    };
    Ok(ExportSettings {
        template,
        ..ExportSettings::default()
    })
}

fn uv_mask_command(args: &[String]) -> Result<bool, String> {
    if !(3..=4).contains(&args.len()) {
        return Err(format!(
            "uv-mask expects a project, output directory, and optional texture-set ID\n\n{HELP}"
        ));
    }
    let session = open(Path::new(&args[1]))?;
    let selected = args
        .get(3)
        .map(|s| uuid::Uuid::parse_str(s).map_err(|e| format!("invalid texture-set UUID: {e}")))
        .transpose()?;
    let sets: Vec<_> = session
        .document()
        .texture_sets
        .iter()
        .filter(|s| selected.is_none_or(|id| s.id.0 == id))
        .map(|s| (s.id, s.name.clone(), s.material_slot, s.resolution))
        .collect();
    if sets.is_empty() {
        return Err("no texture set matches the requested ID".into());
    }
    let out_dir = Path::new(&args[2]);
    std::fs::create_dir_all(out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    for (_, name, slot, size) in sets {
        ensure_size(size)?;
        let covered = subcraft_export::coverage(&session.scene().mesh, slot, size);
        let mut pixels = Vec::with_capacity(covered.len());
        pixels.extend(covered.iter().map(|&b| if b { 1.0f32 } else { 0.0 }));
        let filename = format!("{}_slot{}_uv-mask.png", sanitize(&name), slot);
        let path = out_dir.join(filename);
        subcraft_image::io::save_image(&path, size, size, 1, &pixels, FileFormat::Png8)
            .map_err(|e| e.to_string())?;
        let count = covered.iter().filter(|&&v| v).count();
        println!(
            "Wrote {} ({} / {} covered texels)",
            path.display(),
            count,
            covered.len()
        );
    }
    Ok(true)
}

fn bake_command(args: &[String]) -> Result<bool, String> {
    if args.len() < 3 {
        return Err(format!(
            "bake-mesh-maps expects a project and output directory\n\n{HELP}"
        ));
    }
    let mut settings = BakeSettings::default();
    let mut selected_maps: Option<Vec<MeshMapKind>> = None;
    let mut high_mesh = None;
    let mut index = 3;
    while index < args.len() {
        match args[index].as_str() {
            "--high-mesh" => {
                high_mesh = Some(std::path::PathBuf::from(value(
                    args,
                    &mut index,
                    "--high-mesh",
                )?));
            }
            "--size" => {
                settings.size = value(args, &mut index, "--size")?
                    .parse()
                    .map_err(|e| format!("invalid --size: {e}"))?;
            }
            "--ao-samples" => {
                settings.ao_samples = value(args, &mut index, "--ao-samples")?
                    .parse()
                    .map_err(|e| format!("invalid --ao-samples: {e}"))?;
            }
            "--maps" => {
                selected_maps = Some(parse_maps(value(args, &mut index, "--maps")?)?);
            }
            "--ao-distance" => {
                settings.ao_distance =
                    finite_positive(value(args, &mut index, "--ao-distance")?, "AO distance")?;
            }
            "--thickness-samples" => {
                settings.thickness_samples = value(args, &mut index, "--thickness-samples")?
                    .parse()
                    .map_err(|e| format!("invalid --thickness-samples: {e}"))?;
            }
            "--thickness-distance" => {
                settings.thickness_distance = finite_positive(
                    value(args, &mut index, "--thickness-distance")?,
                    "thickness distance",
                )?;
            }
            "--curvature-radius" => {
                settings.curvature_radius = finite_positive(
                    value(args, &mut index, "--curvature-radius")?,
                    "curvature radius",
                )?;
            }
            "--cage-front" => {
                settings.cage_front =
                    finite_nonnegative(value(args, &mut index, "--cage-front")?, "cage front")?;
            }
            "--cage-rear" => {
                settings.cage_rear =
                    finite_nonnegative(value(args, &mut index, "--cage-rear")?, "cage rear")?;
            }
            other => return Err(format!("unknown bake-mesh-maps option {other:?}\n\n{HELP}")),
        }
        index += 1;
    }
    ensure_size(settings.size)?;
    if settings.ao_samples == 0 || settings.ao_samples > MAX_BAKE_SAMPLES {
        return Err(format!("--ao-samples must be in 1..={MAX_BAKE_SAMPLES}"));
    }
    if settings.thickness_samples == 0 || settings.thickness_samples > MAX_BAKE_SAMPLES {
        return Err(format!(
            "thickness sample count must be in 1..={MAX_BAKE_SAMPLES}"
        ));
    }
    if let Some(maps) = selected_maps {
        settings.maps = maps;
    }
    if settings.maps.is_empty() {
        return Err("at least one map must be selected".into());
    }
    let output_bytes = u64::from(settings.size)
        .checked_mul(u64::from(settings.size))
        .and_then(|texels| {
            let components: usize = settings.maps.iter().map(|kind| kind.components()).sum();
            texels.checked_mul(components as u64)?.checked_mul(4)
        })
        .ok_or_else(|| "mesh-map output memory estimate overflow".to_string())?;
    if output_bytes > MAX_BAKE_OUTPUT_BYTES {
        return Err(format!(
            "requested map outputs need {output_bytes} bytes; CLI budget is {MAX_BAKE_OUTPUT_BYTES} bytes; lower --size or select fewer maps"
        ));
    }
    let mut session = open(Path::new(&args[1]))?;
    let pairs: Vec<_> = session
        .document()
        .texture_sets
        .iter()
        .map(|set| (set.id, set.name.clone(), set.material_slot))
        .collect();
    let mut job = session.start_bake(BakeRequest {
        settings: settings.clone(),
        high_poly: high_mesh,
        texture_sets: pairs.iter().map(|(id, _, slot)| (*id, *slot)).collect(),
    });
    let output = Path::new(&args[2]);
    std::fs::create_dir_all(output).map_err(|e| format!("{}: {e}", output.display()))?;
    loop {
        if let Some(result) = job.poll(&mut session) {
            result.map_err(|e| format!("mesh-map bake failed: {e}"))?;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let sets: Vec<_> = session
        .document()
        .texture_sets
        .iter()
        .map(|s| (s.id, s.name.clone(), s.material_slot))
        .collect();
    let mut count = 0;
    for (id, name, slot) in sets {
        let Some(maps) = session.mesh_maps(id) else {
            continue;
        };
        for (kind, image) in maps.iter().filter(|(kind, _)| settings.maps.contains(kind)) {
            let path = output.join(format!(
                "{}_slot{}_{}.png",
                sanitize(&name),
                slot,
                sanitize(kind.key())
            ));
            subcraft_image::io::save_image(
                &path,
                image.width(),
                image.height(),
                image.comps(),
                &image.to_dense(),
                FileFormat::Png8,
            )
            .map_err(|e| e.to_string())?;
            println!("Wrote {}", path.display());
            count += 1;
        }
    }
    if count == 0 {
        return Err("bake completed without producing map files".into());
    }
    Ok(true)
}

fn parse_maps(text: &str) -> Result<Vec<MeshMapKind>, String> {
    let mut out = Vec::new();
    for name in text.split(',') {
        let map = match name.trim().to_ascii_lowercase().as_str() {
            "normal" => MeshMapKind::Normal,
            "world-normal" | "world_normal" => MeshMapKind::WorldSpaceNormal,
            "id" | "material-id" => MeshMapKind::Id,
            "ao" | "ambient-occlusion" | "ambient_occlusion" => MeshMapKind::AmbientOcclusion,
            "curvature" => MeshMapKind::Curvature,
            "position" => MeshMapKind::Position,
            "thickness" => MeshMapKind::Thickness,
            other => {
                return Err(format!(
                    "unknown mesh map {other:?}; available: normal, world-normal, id, ao, curvature, position, thickness"
                ));
            }
        };
        if !out.contains(&map) {
            out.push(map);
        }
    }
    if out.is_empty() {
        return Err("--maps needs at least one map name".into());
    }
    Ok(out)
}

fn value<'a>(args: &'a [String], index: &mut usize, option: &str) -> Result<&'a str, String> {
    *index += 1;
    args.get(*index)
        .map(String::as_str)
        .ok_or_else(|| format!("{option} requires a value"))
}

fn finite_positive(text: &str, label: &str) -> Result<f32, String> {
    let v: f32 = text.parse().map_err(|e| format!("invalid {label}: {e}"))?;
    if !v.is_finite() || v <= 0.0 || v > 1_000_000.0 {
        Err(format!("{label} must be finite and in 0..=1000000"))
    } else {
        Ok(v)
    }
}

fn finite_nonnegative(text: &str, label: &str) -> Result<f32, String> {
    let v: f32 = text.parse().map_err(|e| format!("invalid {label}: {e}"))?;
    if !v.is_finite() || v < 0.0 || v > 1_000_000.0 {
        Err(format!("{label} must be finite and in 0..=1000000"))
    } else {
        Ok(v)
    }
}

fn ensure_size(size: u32) -> Result<(), String> {
    if !(1..=MAX_BAKE_SIZE).contains(&size) {
        Err(format!("texture resolution must be in 1..={MAX_BAKE_SIZE}"))
    } else {
        Ok(())
    }
}

fn sanitize(name: &str) -> String {
    let out: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        "untitled".into()
    } else {
        out
    }
}

fn inspect_brush(path: &Path) -> Result<(), String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_PRESET_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() as u64 > MAX_PRESET_BYTES {
        return Err(format!("brush preset exceeds {} bytes", MAX_PRESET_BYTES));
    }
    let preset: crate::brush_preset::BrushPreset = serde_json::from_slice(&bytes)
        .map_err(|e| format!("{}: invalid brush preset JSON: {e}", path.display()))?;
    if preset.format != crate::brush_preset::FORMAT || !(1..=2).contains(&preset.version) {
        return Err(format!(
            "{}: unsupported SubCraft brush preset format/version",
            path.display()
        ));
    }
    println!(
        "Brush preset: {} | size {:.2} | flow {:.2} | hardness {:.2} | spacing {:.2} | tip {}",
        preset.name,
        preset.settings.size,
        preset.settings.flow,
        preset.settings.hardness,
        preset.settings.spacing,
        preset.tip.as_deref().unwrap_or("Round")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use subcraft_mesh::primitives;

    fn scratch() -> PathBuf {
        std::env::temp_dir().join(format!("subcraft-cli-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn cli_recognizes_commands_and_leaves_gui_launch_switches_alone() {
        assert!(!run(&["--demo".into()]).unwrap());
        assert!(!run(&["--view-2d".into()]).unwrap());
        assert!(run(&["--help".into()]).unwrap());
        assert!(run(&["--version".into()]).unwrap());
        assert!(run(&["spp-inspect".into(), "test.spp".into()]).is_err());
    }

    #[test]
    fn import_validate_and_export_leave_the_input_project_unchanged() {
        let dir = scratch();
        std::fs::create_dir_all(&dir).unwrap();
        let mesh = primitives::cube(1.0);
        let mut session = Session::new(mesh, 64);
        let source = dir.join("source.subcraft");
        session.save(Some(&source)).unwrap();
        let before = std::fs::read(&source).unwrap();
        validate(&source).unwrap();
        inspect(&source).unwrap();
        let output = dir.join("export");
        let args = vec![
            "export".into(),
            source.to_string_lossy().into_owned(),
            output.to_string_lossy().into_owned(),
        ];
        assert!(run(&args).unwrap());
        assert!(
            !std::fs::read_dir(&output)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
                .is_empty()
        );
        assert_eq!(std::fs::read(&source).unwrap(), before);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn parser_bounds_bake_settings_and_accepts_named_maps() {
        assert_eq!(
            parse_maps("normal,ao,normal").unwrap(),
            vec![MeshMapKind::Normal, MeshMapKind::AmbientOcclusion]
        );
        assert!(parse_maps("normal,pretend").is_err());
        assert!(ensure_size(MAX_BAKE_SIZE + 1).is_err());
        assert!(finite_positive("NaN", "distance").is_err());
        assert!(finite_positive("1000001", "distance").is_err());
    }

    #[test]
    fn uv_mask_emits_all_sets_without_changing_source() {
        let dir = scratch();
        std::fs::create_dir_all(&dir).unwrap();
        let mut session = Session::new(primitives::cube(1.0), 64);
        let source = dir.join("source.subcraft");
        session.save(Some(&source)).unwrap();
        let before = std::fs::read(&source).unwrap();
        let output = dir.join("uv");
        let args = vec![
            "uv-mask".into(),
            source.to_string_lossy().into_owned(),
            output.to_string_lossy().into_owned(),
        ];
        assert!(run(&args).unwrap());
        assert!(
            !std::fs::read_dir(output)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
                .is_empty()
        );
        assert_eq!(std::fs::read(source).unwrap(), before);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bake_selected_named_map_emits_png_without_modifying_source() {
        let dir = scratch();
        std::fs::create_dir_all(&dir).unwrap();
        let mut session = Session::new(primitives::cube(1.0), 64);
        let source = dir.join("source.subcraft");
        session.save(Some(&source)).unwrap();
        let before = std::fs::read(&source).unwrap();
        let output = dir.join("baked");
        let args = vec![
            "bake-mesh-maps".into(),
            source.to_string_lossy().into_owned(),
            output.to_string_lossy().into_owned(),
            "--maps".into(),
            "position".into(),
            "--size".into(),
            "16".into(),
        ];
        assert!(run(&args).unwrap());
        assert!(
            !std::fs::read_dir(&output)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
                .is_empty()
        );
        assert_eq!(std::fs::read(source).unwrap(), before);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
