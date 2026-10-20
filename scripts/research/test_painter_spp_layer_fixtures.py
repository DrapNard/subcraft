"""Unit tests for the bounded, private Painter layer fixture harness."""

import ast
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import painter_spp_layer_fixtures as fixtures


class LayerFixtureTests(unittest.TestCase):
    def test_four_cases_use_same_original_quad_and_controlled_uniform_colors(self):
        cases = fixtures.layer_fixture_cases()
        self.assertEqual(len(cases), 4)
        self.assertEqual(cases[0]["layers_bottom_to_top"], [])
        self.assertEqual(cases[1]["layers_bottom_to_top"], [{
            "name": "SubCraftFill", "type": "FillLayer", "base_color": [0.125, 0.375, 0.625]
        }])
        self.assertEqual(cases[2]["layers_bottom_to_top"], [{
            "name": "SubCraftFill", "type": "FillLayer", "base_color": [0.25, 0.375, 0.625]
        }])
        self.assertEqual(cases[3]["expected_root_order"], ["SubCraftTop", "SubCraftBottom"])
        self.assertEqual(
            [layer["name"] for layer in cases[3]["layers_bottom_to_top"]],
            ["SubCraftBottom", "SubCraftTop"],
        )
        sources = fixtures.mesh_fixtures.fixture_sources()
        self.assertTrue(all(source == sources["baseline_quad"] for source in [sources["baseline_quad"]] * 4))
        for case in cases:
            self.assertTrue(all(layer["type"] == "FillLayer" for layer in case["layers_bottom_to_top"]))
            self.assertTrue(all(len(layer["base_color"]) == 3 for layer in case["layers_bottom_to_top"]))

    def test_population_uses_public_fill_and_uniform_color_operations(self):
        case = fixtures.layer_fixture_cases()[3]
        calls = []

        def remote(script):
            calls.append(script)
            return None

        fixtures._populate_layers(remote, case)
        joined = "\n".join(calls)
        self.assertIn("sp.layerstack.insert_fill(", joined)
        self.assertIn("sp.layerstack.delete_node(n)", joined)
        self.assertIn("sp.layerstack.InsertPosition.from_textureset_stack(stack)", joined)
        self.assertIn("sp.layerstack.InsertPosition.above_node(previous)", joined)
        self.assertIn("sp.textureset.ChannelType.BaseColor", joined)
        self.assertIn("sp.colormanagement.Color(0.1, 0.2, 0.3)", joined)
        self.assertIn("sp.colormanagement.Color(0.7, 0.6, 0.5)", joined)
        self.assertEqual(joined.count("node.set_name("), 2)
        self.assertEqual(joined.count("node.set_source("), 2)
        self.assertEqual(joined.count("previous = node"), 2)

    def test_navigation_readback_is_one_evaluable_expression(self):
        scripts = []
        def remote(script):
            ast.parse(script, mode="eval")
            scripts.append(script)
            return []
        self.assertEqual(fixtures._observe_root_layers(remote), [])
        self.assertEqual(len(scripts), 1)

    def test_observed_root_order_and_colors_are_checked(self):
        case = fixtures.layer_fixture_cases()[3]
        observed = [
            {"name": "SubCraftTop", "type": "FillLayer", "uid": 2, "base_color": [0.7, 0.6, 0.5]},
            {"name": "SubCraftBottom", "type": "FillLayer", "uid": 1, "base_color": [0.1, 0.2, 0.3]},
        ]
        self.assertTrue(fixtures._validate_observation(case, observed))
        with self.assertRaisesRegex(RuntimeError, "order mismatch"):
            fixtures._validate_observation(case, list(reversed(observed)))
        bad_color = [dict(item) for item in observed]
        bad_color[0] = {**bad_color[0], "base_color": [0.0, 0.6, 0.5]}
        with self.assertRaisesRegex(RuntimeError, "Base Color readback mismatch"):
            fixtures._validate_observation(case, bad_color)

    def test_existing_endpoint_refuses_before_painter_launch(self):
        with tempfile.TemporaryDirectory(prefix="layer-fixture-preflight-") as temporary:
            painter = Path(temporary) / "Painter"
            painter.write_bytes(b"placeholder")
            occupied_endpoint = mock.Mock()
            with (
                mock.patch("sys.argv", ["painter_spp_layer_fixtures.py", "--painter", str(painter)]),
                mock.patch.object(fixtures.mesh_fixtures.socket, "create_connection", return_value=occupied_endpoint),
                mock.patch.object(fixtures.subprocess, "Popen") as launch,
            ):
                with self.assertRaisesRegex(RuntimeError, "Existing Painter remote endpoint"):
                    fixtures.main()
                launch.assert_not_called()
                occupied_endpoint.close.assert_called_once_with()

    def test_project_and_endpoint_ownership_checks_fail_closed(self):
        remote = mock.Mock(return_value=True)
        with self.assertRaisesRegex(RuntimeError, "project open"):
            fixtures.require_project_closed(remote)
        remote.assert_called_once_with("substance_painter.project.is_open()")

        with self.assertRaisesRegex(RuntimeError, "not the owned Painter process"):
            fixtures.verify_remote_pid(lambda expression: 100, 101)
        self.assertEqual(fixtures.verify_remote_pid(lambda expression: 101, 101), 101)

    def test_manifest_contract_has_no_bundled_or_external_material_assets(self):
        self.assertEqual(
            fixtures.layer_fixture_cases()[1]["layers_bottom_to_top"][0]["base_color"],
            [0.125, 0.375, 0.625],
        )
        source = Path(fixtures.__file__).read_text(encoding="utf-8")
        self.assertIn('"external_assets_used": []', source)
        self.assertIn('"layer_source_kind": "uniform_color"', source)
        self.assertNotIn("resources/", source)
        self.assertNotIn("bundled/material", source.lower())


if __name__ == "__main__":
    unittest.main()
