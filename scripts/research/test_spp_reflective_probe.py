"""Original synthetic buffers; no Painter assets or fixture archives required."""
import struct
import tempfile
import unittest
from pathlib import Path

from spp_reflective_probe import inspect, MAX_BYTES


def document(*codes):
    name = b'DataOwned'
    record = b'\xff' * 4 + struct.pack('<I', len(name)) + name + struct.pack('<I', len(codes))
    for code in codes:
        key = b'enabled'
        record += struct.pack('<I', len(key)) + key + struct.pack('<I', code)
    data = b'\xdd/|\x1b' + struct.pack('<IIIII', 1, 88, 18, 1, 24 + len(record)) + record
    return data


class ProbeBounds(unittest.TestCase):
    def inspect_bytes(self, data):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / 'owned.bin'
            path.write_bytes(data)
            return inspect(path)

    def test_original_declaration_preserves_unknown_property_code(self):
        result = self.inspect_bytes(document(999))
        self.assertEqual(result['classes'][0].props[0].type_code, 999)
        self.assertEqual(result['_data'], document(999))

    def test_size_magic_and_read_limit(self):
        for data in (b'', b'bad magic' * 8, document(10)[:-1], b'x' * (MAX_BYTES + 1)):
            with self.assertRaises(ValueError):
                self.inspect_bytes(data)

    def test_conflicting_duplicate_schema_is_rejected(self):
        a, b = document(10), document(16)
        both = bytearray(a + b[24:])
        struct.pack_into('<I', both, 20, len(both))
        with self.assertRaisesRegex(ValueError, 'conflicting'):
            self.inspect_bytes(both)


if __name__ == '__main__':
    unittest.main()
