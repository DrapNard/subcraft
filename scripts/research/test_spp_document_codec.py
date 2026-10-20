"""Original synthetic reflective records; no Painter data bundled."""
import struct
import unittest
from spp_document_codec import Reader, Writer

class CodecTests(unittest.TestCase):
    def fixture(self):
        header = bytes.fromhex("dd2f7c1b") + struct.pack("<IIII", 1, 88, 18, 1)
        classes = [("DataTest", [("label",16),("enabled",10),("opacity",1),("child",18)])]
        obj = {"class":"DataTest", "fields":{"label":"Original", "enabled":True, "opacity":0.375, "child":None}}
        writer = Writer(header, classes)
        writer.object(obj)
        return bytes(writer.b)

    def test_exact_roundtrip_and_edit(self):
        data = self.fixture()
        reader = Reader(data)
        obj = reader.object()
        writer = Writer(data, reader.classes)
        writer.object(obj)
        self.assertEqual(bytes(writer.b), data)
        obj["fields"]["label"] = "A longer original label"
        obj["fields"]["enabled"] = False
        writer = Writer(data, reader.classes)
        writer.object(obj)
        edited = Reader(bytes(writer.b)).object()
        self.assertEqual(edited["fields"], obj["fields"])

    def test_opaque16_color_and_channel_tokens_reencode_exactly(self):
        header = bytes.fromhex("dd2f7c1b") + struct.pack("<IIII", 1, 55, 18, 1)
        classes = [("DataTest", [("color",4),("channelTypes",22)])]
        obj = {"class":"DataTest", "fields":{"color":"00"*16, "channelTypes":"01"+"00"*15}}
        writer = Writer(header, classes)
        writer.object(obj)
        data = bytes(writer.b)
        reader = Reader(data)
        decoded = reader.object()
        self.assertEqual(decoded["fields"], obj["fields"])
        writer = Writer(data, reader.classes)
        writer.object(decoded)
        self.assertEqual(bytes(writer.b), data)

    def test_truncation_rejected(self):
        data = self.fixture()
        for size in range(len(data)):
            with self.assertRaises(ValueError):
                Reader(data[:size]).object()

    def test_size_and_pointer_bounds(self):
        with self.assertRaises(ValueError):
            Reader(bytes(65537))
        data = bytearray(self.fixture())
        struct.pack_into("<I",data,20,len(data)+1)
        with self.assertRaises(ValueError):
            Reader(bytes(data)).object()

if __name__ == "__main__": unittest.main()
