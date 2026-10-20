import unittest
from spp_hash_probe import digest, infer_initial

class HashProbeTests(unittest.TestCase):
    def test_public_standard_vectors(self):
        self.assertEqual(digest(b''), (0, 0))
        self.assertEqual(digest(b'hello'), (0xcbd8a7b341bd9b02, 0x5b1e906a48ae1d19))

    def test_inverse_recovers_initial_states_all_tail_lengths(self):
        for length in range(128):
            data = bytes(range(length))
            for initial in [(0,0), (42,42), (0x123456789abcdef0, 0xfedcba9876543210)]:
                self.assertEqual(infer_initial(data, digest(data, initial)), initial)
