"""Synthetic checks for the private Painter mesh fixture harness."""

import tempfile
import unittest
from pathlib import Path
from unittest import mock

import painter_spp_mesh_fixtures as fixtures


def records(text, kind):
    return [line for line in text.splitlines() if line.startswith(kind + " ")]


class MeshFixtureTests(unittest.TestCase):
    def test_controlled_fixture_differences_and_source_counts(self):
        sources = fixtures.fixture_sources()
        baseline = sources["baseline_quad"]
        self.assertEqual(len(sources), 5)
        self.assertEqual(fixtures.source_summary(baseline), {"v": 4, "vt": 4, "vn": 1, "f": 2})
        self.assertEqual(len({fixtures.hashlib.sha256(s.encode()).hexdigest() for s in sources.values()}), 5)

        for name in ("vertex_x", "uv_u", "normal_sign"):
            self.assertEqual(fixtures.source_summary(sources[name]), fixtures.source_summary(baseline))
        self.assertNotEqual(records(sources["vertex_x"], "v"), records(baseline, "v"))
        for kind in ("vt", "vn", "f"):
            self.assertEqual(records(sources["vertex_x"], kind), records(baseline, kind))
        self.assertNotEqual(records(sources["uv_u"], "vt"), records(baseline, "vt"))
        for kind in ("v", "vn", "f"):
            self.assertEqual(records(sources["uv_u"], kind), records(baseline, kind))
        self.assertNotEqual(records(sources["normal_sign"], "vn"), records(baseline, "vn"))
        for kind in ("v", "vt", "f"):
            self.assertEqual(records(sources["normal_sign"], kind), records(baseline, kind))

        triangle = sources["triangle_count"]
        self.assertEqual(fixtures.source_summary(triangle), {"v": 4, "vt": 4, "vn": 1, "f": 1})
        for kind in ("v", "vt", "vn"):
            self.assertEqual(records(triangle, kind), records(baseline, kind))

    def test_structural_fixtures_hold_geometry_constant(self):
        fixtures_by_name = fixtures.structural_fixture_sources()
        self.assertEqual(
            set(fixtures_by_name),
            {
                "structural_single_object",
                "structural_two_objects_shared_material",
                "structural_two_material_slots",
            },
        )
        baseline = fixtures_by_name["structural_single_object"]
        self.assertEqual(
            fixtures.source_summary(baseline["obj"]),
            {"v": 4, "vt": 4, "vn": 1, "f": 2},
        )
        for name, item in fixtures_by_name.items():
            self.assertEqual(fixtures.source_summary(item["obj"]), fixtures.source_summary(baseline["obj"]))
            self.assertEqual(records(item["obj"], "v"), records(baseline["obj"], "v"))
            self.assertEqual(records(item["obj"], "vt"), records(baseline["obj"], "vt"))
            self.assertEqual(records(item["obj"], "vn"), records(baseline["obj"], "vn"))
            self.assertEqual(records(item["obj"], "f"), records(baseline["obj"], "f"))
            self.assertIn("mtllib ", item["obj"])
            self.assertEqual(item["obj"].splitlines()[1], f"mtllib {name}.mtl")
            self.assertEqual(fixtures.mtl_summary(item["mtl"])["newmtl"], fixtures.mtl_summary(item["mtl"])["Kd"])

        two_objects = fixtures_by_name["structural_two_objects_shared_material"]
        self.assertEqual(fixtures.obj_structure_summary(two_objects["obj"]), {"o": 2, "usemtl": 2, "mtllib": 1})
        self.assertEqual(fixtures.mtl_summary(two_objects["mtl"]), {"newmtl": 1, "Kd": 1})
        self.assertEqual(records(two_objects["obj"], "usemtl"), ["usemtl SharedMaterial"] * 2)

        two_slots = fixtures_by_name["structural_two_material_slots"]
        self.assertEqual(fixtures.obj_structure_summary(two_slots["obj"]), {"o": 1, "usemtl": 2, "mtllib": 1})
        self.assertEqual(fixtures.mtl_summary(two_slots["mtl"]), {"newmtl": 2, "Kd": 2})
        self.assertEqual(records(two_slots["obj"], "usemtl"), ["usemtl RedSlot", "usemtl BlueSlot"])
        self.assertIn("Kd 1 0 0", two_slots["mtl"])
        self.assertIn("Kd 0 0 1", two_slots["mtl"])

    def test_cleanup_does_not_close_unowned_project_or_endpoint(self):
        remote = mock.Mock(return_value=True)
        fixtures.close_owned_project(remote, endpoint_owned=False, project_owned=True)
        fixtures.close_owned_project(remote, endpoint_owned=True, project_owned=False)
        remote.assert_not_called()

        fixtures.close_owned_project(remote, endpoint_owned=True, project_owned=True)
        self.assertEqual(remote.call_args_list, [
            mock.call("substance_painter.project.is_open()"),
            mock.call("substance_painter.project.close()"),
        ])

    def test_existing_endpoint_refuses_before_painter_launch(self):
        with tempfile.TemporaryDirectory(prefix="mesh-fixture-preflight-") as temporary:
            painter = Path(temporary) / "Painter"
            painter.write_bytes(b"placeholder")
            occupied_endpoint = mock.Mock()
            with (
                mock.patch("sys.argv", [
                    "painter_spp_mesh_fixtures.py",
                    "--painter", str(painter),
                    "--skip-scene-inspection",
                ]),
                mock.patch.object(fixtures.socket, "create_connection", return_value=occupied_endpoint),
                mock.patch.object(fixtures.subprocess, "Popen") as launch,
            ):
                with self.assertRaisesRegex(RuntimeError, "Existing Painter remote endpoint"):
                    fixtures.main()
                launch.assert_not_called()
                occupied_endpoint.close.assert_called_once_with()


if __name__ == "__main__":
    unittest.main()
