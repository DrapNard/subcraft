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

    def test_fixed_opaque_widths_and_nonzero_type11_roundtrip(self):
        header = bytes.fromhex("dd2f7c1b") + struct.pack("<IIII", 1, 55, 18, 1)
        classes = [("DataPrimitives", [
            ("type2", 2), ("type3", 3), ("type11", 11), ("type14", 14),
        ])]
        obj = {"class": "DataPrimitives", "fields": {
            "type2": "01" * 8,
            "type3": "02" * 12,
            "type11": 0xFEED1234,
            "type14": "03" * 64,
        }}
        writer = Writer(header, classes)
        writer.object(obj)
        data = bytes(writer.b)
        reader = Reader(data)
        decoded = reader.object()
        self.assertEqual(decoded["fields"], obj["fields"])
        writer = Writer(data, reader.classes)
        writer.object(decoded)
        self.assertEqual(bytes(writer.b), data)

    def test_type17_uses_one_shared_class_definition_and_nested_class_refs(self):
        header = bytes.fromhex("dd2f7c1b") + struct.pack("<IIII", 1, 21, 34, 3)
        classes = [
            ("DataRoot", [("items", 17)]),
            ("DataItem", [("uid", 11), ("payload", 18)]),
            ("DataPayload", [("small", 2), ("medium", 3), ("wide", 14)]),
        ]
        def payload(seed):
            return {"class": "DataPayload", "fields": {
                "small": f"{seed:02x}" * 8,
                "medium": f"{seed + 1:02x}" * 12,
                "wide": f"{seed + 2:02x}" * 64,
            }}
        obj = {"class": "DataRoot", "fields": {"items": [
            {"class": "DataItem", "fields": {"uid": 7, "payload": payload(10)}},
            {"class": "DataItem", "fields": {"uid": 0xF0000001, "payload": payload(20)}},
        ]}}
        writer = Writer(header, classes)
        writer.object(obj)
        data = bytes(writer.b)
        reader = Reader(data)
        decoded = reader.object()
        decoded_items = decoded["fields"]["items"]
        expected_items = obj["fields"]["items"]
        self.assertEqual(len(decoded_items), 2)
        for actual, expected in zip(decoded_items, expected_items):
            self.assertEqual(actual["class"], expected["class"])
            self.assertEqual(actual["fields"]["uid"], expected["fields"]["uid"])
            self.assertEqual(
                actual["fields"]["payload"]["fields"],
                expected["fields"]["payload"]["fields"],
            )
        self.assertEqual(len(reader.classes), 3)
        writer = Writer(data, reader.classes)
        writer.object(decoded)
        self.assertEqual(bytes(writer.b), data)

    def test_type17_writer_rejects_heterogeneous_class_array(self):
        header = bytes.fromhex("dd2f7c1b") + struct.pack("<IIII", 1, 21, 34, 3)
        classes = [
            ("DataRoot", [("items", 17)]),
            ("DataA", [("value", 11)]),
            ("DataB", [("value", 11)]),
        ]
        obj = {"class": "DataRoot", "fields": {"items": [
            {"class": "DataA", "fields": {"value": 1}},
            {"class": "DataB", "fields": {"value": 2}},
        ]}}
        writer = Writer(header, classes)
        with self.assertRaisesRegex(ValueError, "heterogeneous"):
            writer.object(obj)

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
