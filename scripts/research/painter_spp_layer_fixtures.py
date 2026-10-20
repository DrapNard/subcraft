#!/usr/bin/env python3
"""Save private Painter SPP fixtures for clean-room layer research.

The only layer sources are public Painter API uniform colors. Each fixture is
created from the same tiny authored quad in a fresh project and saved in Full
mode. No Painter bundled assets are loaded or copied. The output directory is
private and must not be committed. This harness records observations; it does
not implement or claim general SPP layer import/export support.

API references:
* https://experienceleague.adobe.com/en/docs/substance-3d-dev/painter-python/api/substancepainter-package/source-module/uniformcolor
* https://experienceleague.adobe.com/en/docs/substance-3d-dev/painter-python/api/substancepainter-package/layerstack-module/edition
* https://experienceleague.adobe.com/en/docs/substance-3d-dev/painter-python/api/substancepainter-package/layerstack-module/navigation
"""

import argparse
import base64
import hashlib
import http.client
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

import painter_spp_mesh_fixtures as mesh_fixtures


MAX_REQUEST_BYTES = 64 * 1024
MAX_RESPONSE_BYTES = 64 * 1024
DEFAULT_TIMEOUT_SECONDS = 300


def layer_fixture_cases():
    """Declarative expectations for the four controlled layer-stack cases."""
    return [
        {
            "name": "blank_baseline",
            "layers_bottom_to_top": [],
            "expected_root_order": [],
        },
        {
            "name": "single_fill_basecolor_0125_0375_0625",
            "layers_bottom_to_top": [
                {"name": "SubCraftFill", "type": "FillLayer", "base_color": [0.125, 0.375, 0.625]},
            ],
            "expected_root_order": ["SubCraftFill"],
        },
        {
            "name": "single_fill_basecolor_025_0375_0625",
            "layers_bottom_to_top": [
                {"name": "SubCraftFill", "type": "FillLayer", "base_color": [0.25, 0.375, 0.625]},
            ],
            "expected_root_order": ["SubCraftFill"],
        },
        {
            "name": "two_named_fill_layers_order",
            "layers_bottom_to_top": [
                {"name": "SubCraftBottom", "type": "FillLayer", "base_color": [0.1, 0.2, 0.3]},
                {"name": "SubCraftTop", "type": "FillLayer", "base_color": [0.7, 0.6, 0.5]},
            ],
            "expected_root_order": ["SubCraftTop", "SubCraftBottom"],
        },
    ]


def verify_remote_pid(remote, process_pid):
    """Prove the endpoint belongs to this launcher before taking ownership."""
    actual = remote("__import__('os').getpid()")
    if actual != process_pid:
        raise RuntimeError("Remote endpoint is not the owned Painter process")
    return actual


def require_project_closed(remote):
    """Refuse to replace a project that was open before this harness ran."""
    if remote("substance_painter.project.is_open()"):
        raise RuntimeError("Painter already has a project open; refusing to replace it")


def _float_literal(value):
    return repr(float(value))


def _insert_layer_source(spec, is_first):
    color = ", ".join(_float_literal(value) for value in spec["base_color"])
    if is_first:
        position = "sp.layerstack.InsertPosition.from_textureset_stack(stack)"
    else:
        position = "sp.layerstack.InsertPosition.above_node(previous)"
    return (
        "stack = sp.textureset.get_active_stack(); "
        f"node = sp.layerstack.insert_fill({position}); "
        f"node.set_name({spec['name']!r}); "
        "node.set_source(sp.textureset.ChannelType.BaseColor, "
        f"sp.colormanagement.Color({color})); "
        "previous = node"
    )


def _populate_layers(remote, case):
    operations = [
        "import substance_painter as sp",
        # Only called after this harness creates its fresh owned project. Remove its default layer.
        "[sp.layerstack.delete_node(n) for n in sp.layerstack.get_root_layer_nodes(sp.textureset.get_active_stack())]",
    ]
    operations.extend(
        _insert_layer_source(spec, is_first=(index == 0))
        for index, spec in enumerate(case["layers_bottom_to_top"])
    )
    if not case["layers_bottom_to_top"]:
        operations.append("sp.textureset.get_active_stack()")
    remote("; ".join(operations))


def _observe_root_layers(remote):
    # Remote scripting evaluates a single expression; multi-statement scripts return None.
    script = (
        "[{ 'name': node.get_name(), 'type': node.get_type().name, "
        "'uid': node.uid(), 'base_color': ("
        "list(node.get_source(substance_painter.textureset.ChannelType.BaseColor).get_color().value) "
        "if node.get_type().name == 'FillLayer' else None) } "
        "for node in substance_painter.layerstack.get_root_layer_nodes("
        "substance_painter.textureset.get_active_stack())]"
    )
    observed = remote(script)
    if not isinstance(observed, list):
        raise RuntimeError("Painter layer navigation API returned a non-list result")
    return observed


def _validate_observation(case, observed):
    expected = case["layers_bottom_to_top"]
    expected_root = case["expected_root_order"]
    names = [item.get("name") for item in observed]
    if names != expected_root:
        raise RuntimeError(f"Root layer order mismatch for {case['name']}: {names!r}")
    by_name = {item["name"]: item for item in observed}
    if len(by_name) != len(observed):
        raise RuntimeError(f"Duplicate root layer names for {case['name']}")
    for spec in expected:
        actual = by_name.get(spec["name"])
        if actual is None or actual.get("type") != spec["type"]:
            raise RuntimeError(f"Layer type/name mismatch for {case['name']}: {spec['name']}")
        color = actual.get("base_color")
        if not isinstance(color, list) or len(color) < 3:
            raise RuntimeError(f"Missing Base Color readback for {case['name']}: {spec['name']}")
        if any(abs(float(color[i]) - spec["base_color"][i]) > 1e-6 for i in range(3)):
            raise RuntimeError(f"Base Color readback mismatch for {case['name']}: {spec['name']}")
    return True


def _hash_file(path):
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as stream:
        while chunk := stream.read(64 * 1024):
            size += len(chunk)
            if size > mesh_fixtures.MAX_SPP_BYTES:
                raise RuntimeError("SPP fixture exceeds configured size bound")
            digest.update(chunk)
    return {"name": path.name, "bytes": size, "sha256": digest.hexdigest()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--painter", required=True, type=Path)
    parser.add_argument("--timeout", type=int, default=DEFAULT_TIMEOUT_SECONDS)
    args = parser.parse_args()
    if not args.painter.is_file():
        raise RuntimeError("Painter executable does not exist")
    if not 30 <= args.timeout <= 900:
        raise RuntimeError("Timeout must be between 30 and 900 seconds")

    mesh_fixtures.check_endpoint_closed()
    root = Path(tempfile.mkdtemp(prefix="subcraft-spp-layer-fixtures-"))
    mesh_path = root / "same_original_quad.obj"
    mesh_bytes = mesh_fixtures.fixture_sources()["baseline_quad"].encode("utf-8")
    mesh_path.write_bytes(mesh_bytes)
    source_record = {
        "name": mesh_path.name,
        "bytes": len(mesh_bytes),
        "sha256": hashlib.sha256(mesh_bytes).hexdigest(),
        "counts": mesh_fixtures.source_summary(mesh_bytes.decode("utf-8")),
    }
    print(json.dumps({"stage": "private_fixture_directory", "root": str(root)}), flush=True)

    deadline = time.monotonic() + args.timeout
    launch_env = mesh_fixtures.set_low_impact_environment()
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
        if len(body) > MAX_REQUEST_BYTES:
            raise RuntimeError("Oversized Painter API request")
        client = http.client.HTTPConnection(
            mesh_fixtures.ENDPOINT_HOST,
            mesh_fixtures.ENDPOINT_PORT,
            timeout=min(10, remaining),
        )
        try:
            client.request("POST", "/run.json", body, {"Content-Type": "application/json"})
            response = client.getresponse()
            data = response.read(MAX_RESPONSE_BYTES + 1)
            if len(data) > MAX_RESPONSE_BYTES:
                raise RuntimeError("Oversized Painter API response")
            if response.status != 200:
                raise RuntimeError(f"Painter API status {response.status}")
            value = json.loads(data)
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
        verify_remote_pid(remote, process.pid)
        owns_endpoint = True
        remote("import substance_painter")
        api_version = remote("substance_painter.__version__")
        require_project_closed(remote)

        for case in layer_fixture_cases():
            if process.poll() is not None:
                raise RuntimeError(f"Owned Painter process exited: {process.returncode}")
            require_project_closed(remote)
            remote(
                f"substance_painter.project.create(mesh_file_path={str(mesh_path)!r}, "
                "settings=substance_painter.project.Settings(default_texture_resolution=128))"
            )
            owns_project = True
            wait_for(editable)
            remote("import substance_painter as sp; sp.textureset.get_active_stack()")
            _populate_layers(remote, case)
            observed = _observe_root_layers(remote)
            _validate_observation(case, observed)

            output = root / f"{case['name']}.spp"
            remote(
                f"substance_painter.project.save_as({str(output)!r}, "
                "substance_painter.project.ProjectSaveMode.Full)"
            )
            wait_for(editable)
            records.append({
                "fixture": case["name"],
                "source_obj": source_record,
                "expected_layers_bottom_to_top": case["layers_bottom_to_top"],
                "expected_root_order": case["expected_root_order"],
                "observed_root_layers": observed,
                "spp": _hash_file(output),
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
                "private clean-room SPP layer fixtures using only public uniform-color "
                "sources; not an SPP layer importer/exporter support claim"
            ),
            "painter_api_version": api_version,
            "source_mesh": source_record,
            "external_assets_used": [],
            "layer_source_kind": "uniform_color",
            "limits": {
                "max_spp_bytes": mesh_fixtures.MAX_SPP_BYTES,
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
            mesh_fixtures.close_owned_project(remote, owns_endpoint, owns_project)
            process.terminate()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


if __name__ == "__main__":
    main()
