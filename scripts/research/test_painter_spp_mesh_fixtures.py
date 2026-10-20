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
