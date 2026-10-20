#!/usr/bin/env python3
"""Generate owned, minimal Painter SPP mesh fixtures for clean-room research.

Each fixture starts from a fresh project and receives a full save. The harness
records the original OBJ, archive and /paint/scene.assbin hashes and bounded
metadata only. It is an oracle harness, not an Assbin parser or SPP importer.
All outputs stay in a private temporary directory; do not commit them.
"""

import argparse
import base64
import hashlib
import http.client
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time


ENDPOINT_HOST = "127.0.0.1"
ENDPOINT_PORT = 60041
MAX_RESPONSE_BYTES = 64 * 1024
MAX_SPP_BYTES = 128 * 1024 * 1024
MAX_SCENE_BYTES = 64 * 1024 * 1024
MAX_SCENE_RANK = 3
DEFAULT_TIMEOUT_SECONDS = 300


BASE_VERTICES = [
    (-1.0, -1.0, 0.0),
    (1.0, -1.0, 0.0),
    (1.0, 1.0, 0.0),
    (-1.0, 1.0, 0.0),
]
BASE_UVS = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]
BASE_NORMALS = [(0.0, 0.0, 1.0)]
BASE_FACES = [(1, 2, 3), (1, 3, 4)]


def _obj(vertices, uvs, normals, faces):
    lines = ["# Original SubCraft SPP mesh fixture", "o SubCraftMeshFixture"]
    lines.extend("v {:.9g} {:.9g} {:.9g}".format(*point) for point in vertices)
    lines.extend("vt {:.9g} {:.9g}".format(*uv) for uv in uvs)
    lines.extend("vn {:.9g} {:.9g} {:.9g}".format(*normal) for normal in normals)
    lines.extend("f " + " ".join(f"{i}/{i}/1" for i in face) for face in faces)
    return "\n".join(lines) + "\n"


def fixture_sources():
    """Five independently authored files: baseline plus one controlled edit each."""
    baseline = _obj(BASE_VERTICES, BASE_UVS, BASE_NORMALS, BASE_FACES)

    changed_vertices = list(BASE_VERTICES)
    changed_vertices[1] = (1.125, -1.0, 0.0)
    changed_uvs = list(BASE_UVS)
    changed_uvs[2] = (0.875, 1.0)
    changed_normals = [(0.0, 0.0, -1.0)]

    return {
        "baseline_quad": baseline,
        "vertex_x": _obj(changed_vertices, BASE_UVS, BASE_NORMALS, BASE_FACES),
        "uv_u": _obj(BASE_VERTICES, changed_uvs, BASE_NORMALS, BASE_FACES),
        "normal_sign": _obj(BASE_VERTICES, BASE_UVS, changed_normals, BASE_FACES),
        "triangle_count": _obj(BASE_VERTICES, BASE_UVS, BASE_NORMALS, [BASE_FACES[0]]),
    }


def structural_fixture_sources():
    """Same quad arranged to isolate object hierarchy and material assignment."""
    vertices = "\n".join(
        "v {:.9g} {:.9g} {:.9g}".format(*point) for point in BASE_VERTICES
    )
    uvs = "\n".join("vt {:.9g} {:.9g}".format(*uv) for uv in BASE_UVS)
    normals = "\n".join(
        "vn {:.9g} {:.9g} {:.9g}".format(*normal) for normal in BASE_NORMALS
    )
    common = f"{vertices}\n{uvs}\n{normals}\n"
    tri_a = "f 1/1/1 2/2/1 3/3/1"
    tri_b = "f 1/1/1 3/3/1 4/4/1"

    def obj(mtl_name, body):
        return (
            "# Original SubCraft structural SPP fixture\n"
            f"mtllib {mtl_name}\n"
            f"{common}{body}\n"
        )

    def mtl(materials):
        lines = ["# Original SubCraft structural MTL fixture"]
        for name, color in materials:
            lines.extend((f"newmtl {name}", "Kd {:.6g} {:.6g} {:.6g}".format(*color)))
        return "\n".join(lines) + "\n"

    shared_mtl = [("SharedMaterial", (0.5, 0.5, 0.5))]
    return {
        "structural_single_object": {
            "obj": obj(
                "structural_single_object.mtl",
                f"o SingleObject\nusemtl SharedMaterial\n{tri_a}\n{tri_b}",
            ),
            "mtl": mtl(shared_mtl),
        },
        "structural_two_objects_shared_material": {
            "obj": obj(
                "structural_two_objects_shared_material.mtl",
                f"o ObjectA\nusemtl SharedMaterial\n{tri_a}\n"
                f"o ObjectB\nusemtl SharedMaterial\n{tri_b}",
            ),
            "mtl": mtl(shared_mtl),
        },
        "structural_two_material_slots": {
            "obj": obj(
                "structural_two_material_slots.mtl",
                f"o SingleObject\nusemtl RedSlot\n{tri_a}\n"
                f"usemtl BlueSlot\n{tri_b}",
            ),
            "mtl": mtl([
                ("RedSlot", (1.0, 0.0, 0.0)),
                ("BlueSlot", (0.0, 0.0, 1.0)),
            ]),
        },
    }


def source_summary(text):
    counts = {"v": 0, "vt": 0, "vn": 0, "f": 0}
    for line in text.splitlines():
        head = line.partition(" ")[0]
        if head in counts:
            counts[head] += 1
    return counts


def obj_structure_summary(text):
    counts = {"o": 0, "usemtl": 0, "mtllib": 0}
    for line in text.splitlines():
        head = line.partition(" ")[0]
        if head in counts:
            counts[head] += 1
    return counts


def mtl_summary(text):
    counts = {"newmtl": 0, "Kd": 0}
    for line in text.splitlines():
        head = line.partition(" ")[0]
        if head in counts:
            counts[head] += 1
    return counts


def set_low_impact_environment():
    """Limit the launcher and Painter to low priority and four Linux CPUs."""
    if hasattr(os, "sched_getaffinity"):
        available = sorted(os.sched_getaffinity(0))
        if available:
            os.sched_setaffinity(0, available[:4])
    if hasattr(os, "nice"):
        try:
            os.nice(19)
        except OSError:
            pass

    env = os.environ.copy()
    for key in (
        "OMP_NUM_THREADS",
        "OPENBLAS_NUM_THREADS",
        "MKL_NUM_THREADS",
        "NUMEXPR_MAX_THREADS",
        "RAYON_NUM_THREADS",
    ):
        env[key] = "4"
    return env


def inspect_file(path):
    digest = hashlib.sha256()
    count = 0
    with path.open("rb") as stream:
        header = stream.read(64)
        stream.seek(0)
        while chunk := stream.read(64 * 1024):
            count += len(chunk)
            if count > MAX_SPP_BYTES:
                raise RuntimeError(f"Fixture exceeds {MAX_SPP_BYTES} byte budget")
            digest.update(chunk)
    return {
        "name": path.name,
        "bytes": count,
        "sha256": digest.hexdigest(),
        "header_hex": header.hex(),
    }


def inspect_scene_dataset(path):
    """Read only the bounded U8 scene dataset, never parse or rewrite its format."""
    try:
        import h5py
    except ImportError as error:
        raise RuntimeError("h5py is required to hash /paint/scene.assbin") from error

    with h5py.File(str(path), "r") as container:
        dataset_path = "/paint/scene.assbin"
        if dataset_path not in container:
            raise RuntimeError(f"Expected dataset is absent: {dataset_path}")
        dataset = container[dataset_path]
        if not isinstance(dataset, h5py.Dataset):
            raise RuntimeError(f"Expected an HDF5 dataset: {dataset_path}")
        if dataset.ndim > MAX_SCENE_RANK:
            raise RuntimeError(f"Unexpected scene dataset rank: {dataset.ndim}")
        if dataset.dtype.kind != "u" or dataset.dtype.itemsize != 1:
            raise RuntimeError(f"Expected U8 scene bytes, got {dataset.dtype}")

        element_count = 1
        for dimension in dataset.shape:
            if dimension < 0 or dimension > MAX_SCENE_BYTES:
                raise RuntimeError("Scene dataset dimension exceeds its bound")
            element_count *= dimension
            if element_count > MAX_SCENE_BYTES:
                raise RuntimeError("Scene dataset exceeds its byte bound")
        if element_count > MAX_SCENE_BYTES:
            raise RuntimeError("Scene dataset exceeds its byte bound")

        payload = bytes(dataset[()])
        if len(payload) != element_count:
            raise RuntimeError("Scene dataset length differs from its bounded shape")
        if len(payload) < 4 or not payload.startswith(b"ASSIMP.binary-dump."):
            raise RuntimeError("Scene dataset lacks the observed Assimp dump signature")

        attrs = {}
        if "m3_x64_128" in dataset.attrs:
            value = dataset.attrs["m3_x64_128"]
            flat = value.reshape(-1).tolist() if hasattr(value, "reshape") else [value]
            if len(flat) > 16:
                raise RuntimeError("Scene checksum attribute exceeds its value bound")
            attrs["m3_x64_128"] = flat

        return {
            "path": dataset_path,
            "shape": list(dataset.shape),
            "dtype": str(dataset.dtype),
            "bytes": len(payload),
            "sha256": hashlib.sha256(payload).hexdigest(),
            "prefix_hex": payload[:64].hex(),
            "attributes": attrs,
        }


def check_endpoint_closed():
    try:
        connection = socket.create_connection((ENDPOINT_HOST, ENDPOINT_PORT), timeout=1)
    except OSError:
        return
    connection.close()
    raise RuntimeError("Existing Painter remote endpoint: refusing to touch its project")


def close_owned_project(remote, endpoint_owned, project_owned):
    """Close only a project this harness created on its verified endpoint."""
    if not endpoint_owned or not project_owned:
        return
    try:
        if remote("substance_painter.project.is_open()"):
            remote("substance_painter.project.close()")
    except Exception:
        pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--painter", required=True, type=Path)
    parser.add_argument("--timeout", type=int, default=DEFAULT_TIMEOUT_SECONDS)
    parser.add_argument(
        "--skip-scene-inspection",
        action="store_true",
        help="Save fixture-only SPPs; leave scene_assbin null for external h5dump inspection",
    )
    parser.add_argument(
        "--structural",
        action="store_true",
        help=(
            "also save a controlled same-quad group varying OBJ object hierarchy "
            "and named MTL material slots"
        ),
    )
    args = parser.parse_args()
    if not args.painter.is_file():
        raise RuntimeError("Painter executable does not exist")
    if not 30 <= args.timeout <= 900:
        raise RuntimeError("Timeout must be between 30 and 900 seconds")
    if not args.skip_scene_inspection:
        try:
            import h5py  # noqa: F401
        except ImportError as error:
            raise RuntimeError(
                "h5py is required unless --skip-scene-inspection is set"
            ) from error
    check_endpoint_closed()

    root = Path(tempfile.mkdtemp(prefix="subcraft-spp-mesh-fixtures-"))
    print(json.dumps({"stage": "private_fixture_directory", "root": str(root)}), flush=True)
    fixture_specs = {
        name: {"obj": text, "mtl": None}
        for name, text in fixture_sources().items()
    }
    if args.structural:
        fixture_specs.update(structural_fixture_sources())
    obj_paths = {}
    source_records = {}
    for name, spec in fixture_specs.items():
        text = spec["obj"]
        path = root / f"{name}.obj"
        encoded = text.encode("utf-8")
        path.write_bytes(encoded)
        obj_paths[name] = path
        source_records[name] = {
            "name": path.name,
            "bytes": len(encoded),
            "sha256": hashlib.sha256(encoded).hexdigest(),
            "counts": source_summary(text),
        }
        if spec["mtl"] is not None:
            mtl_path = root / f"{name}.mtl"
            mtl_encoded = spec["mtl"].encode("utf-8")
            mtl_path.write_bytes(mtl_encoded)
            source_records[name]["obj_structure_counts"] = obj_structure_summary(text)
            source_records[name]["source_mtl"] = {
                "name": mtl_path.name,
                "bytes": len(mtl_encoded),
                "sha256": hashlib.sha256(mtl_encoded).hexdigest(),
                "counts": mtl_summary(spec["mtl"]),
            }

    deadline = time.monotonic() + args.timeout
    launch_env = set_low_impact_environment()
    creationflags = 0
    if os.name == "nt":
        creationflags = getattr(subprocess, "BELOW_NORMAL_PRIORITY_CLASS", 0)
    process = subprocess.Popen(
        [str(args.painter), "--enable-remote-scripting", "--disable-version-checking"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        env=launch_env,
        creationflags=creationflags,
    )

    def remote(script):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("Fixture operation deadline exceeded")
        body = json.dumps({"python": base64.b64encode(script.encode()).decode()})
        if len(body) > MAX_RESPONSE_BYTES:
            raise RuntimeError("Oversized Painter API request")
        client = http.client.HTTPConnection(
            ENDPOINT_HOST, ENDPOINT_PORT, timeout=min(10, remaining)
        )
        try:
            client.request(
                "POST", "/run.json", body,
                {"Content-Type": "application/json"},
            )
            response = client.getresponse()
            response_body = response.read(MAX_RESPONSE_BYTES + 1)
            if len(response_body) > MAX_RESPONSE_BYTES:
                raise RuntimeError("Oversized Painter API response")
            if response.status != 200:
                raise RuntimeError(f"Painter API status {response.status}")
            value = json.loads(response_body)
            if isinstance(value, dict) and "error" in value:
                raise RuntimeError(f"Painter API error: {value['error']}")
            return value
        finally:
            client.close()

    owns_endpoint = False
    owns_project = False

    def wait_for(predicate, expected=True):
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise RuntimeError(f"Owned Painter process exited: {process.returncode}")
            try:
                if remote(predicate) == expected:
                    return
            except (OSError, http.client.HTTPException, json.JSONDecodeError):
                pass
            time.sleep(0.25)
        raise TimeoutError("Painter did not reach the requested project state")

    editable = (
        "substance_painter.project.is_open() and "
        "not substance_painter.project.is_busy() and "
        "substance_painter.project.is_in_edition_state()"
    )
    records = []
    try:
        wait_for("True")
        if remote("__import__('os').getpid()") != process.pid:
            raise RuntimeError("Remote endpoint is not the owned Painter process")
        owns_endpoint = True
        remote("import substance_painter")
        api_version = remote("substance_painter.__version__")
        if remote("substance_painter.project.is_open()"):
            raise RuntimeError("Painter already has a project open; refusing to replace it")

        for name, mesh_path in obj_paths.items():
            if process.poll() is not None:
                raise RuntimeError(f"Owned Painter process exited: {process.returncode}")
            if remote("substance_painter.project.is_open()"):
                raise RuntimeError("A project is already open before fresh fixture creation")

            remote(
                f"substance_painter.project.create(mesh_file_path={str(mesh_path)!r}, "
                "settings=substance_painter.project.Settings(default_texture_resolution=128))"
            )
            owns_project = True
            wait_for(editable)
            output = root / f"{name}.spp"
            remote(
                f"substance_painter.project.save_as({str(output)!r}, "
                "substance_painter.project.ProjectSaveMode.Full)"
            )
            wait_for(editable)
            archive = inspect_file(output)
            scene = None if args.skip_scene_inspection else inspect_scene_dataset(output)
            records.append({
                "fixture": name,
                "source_obj": source_records[name],
                "spp": archive,
                "scene_assbin": scene,
                "painter_saved_version": remote(
                    "substance_painter.project.last_saved_substance_painter_version()"
                ),
                "save_mode": "Full",
            })
            remote("substance_painter.project.close()")
            wait_for("not substance_painter.project.is_open()")
            owns_project = False

        manifest = {
            "purpose": (
                "private clean-room differential fixture corpus; hashes and bounded "
                "metadata only; not an Assbin parser or SPP support claim"
            ),
            "painter_api_version": api_version,
            "scene_dataset": "/paint/scene.assbin",
            "scene_inspection_skipped": args.skip_scene_inspection,
            "structural_fixtures_included": args.structural,
            "limits": {
                "max_spp_bytes": MAX_SPP_BYTES,
                "max_scene_bytes": MAX_SCENE_BYTES,
                "request_deadline_seconds": args.timeout,
                "launcher_priority": "nice 19; Windows below-normal priority",
                "thread_environment_limit": 4,
            },
            "fixtures": records,
        }
        (root / "manifest.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")
        print(json.dumps({"root": str(root), **manifest}), flush=True)
    finally:
        if process.poll() is None:
            close_owned_project(remote, owns_endpoint, owns_project)
            process.terminate()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


if __name__ == "__main__":
    main()
