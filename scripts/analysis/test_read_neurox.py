"""Reader regression checks, including an optional Rust-exported fixture."""
import json
import os
import tempfile
import unittest
from pathlib import Path

import numpy as np

from read_neurox import read_neurox


class ReaderTests(unittest.TestCase):
    def fixture(self, root):
        parameters = {"schemaVersion": 1, "complete": True, "samplesPerChannel": 3,
                      "channelCount": 2, "sampleRateHz": 30000,
                      "firstSampleIndex": 100, "adcZero": 32768,
                      "microvoltsPerAdc": .195,
                      "channels": [{"label": "B-09"}, {"label": "C-20"}]}
        (root / "parameters.json").write_text(json.dumps(parameters), encoding="utf-8")
        np.array([[32768, 32769], [0, 65535], [32767, 32770]], dtype="<u2").tofile(root / "samples.u16le")
        np.array([100, 101, 200], dtype="<u8").tofile(root / "sample_index.u64le")
        np.array([1, 2, 0], dtype="<u2").tofile(root / "ttl_in.u16le")
        (root / "blocks.csv").write_text(
            "sequence,row_start,sample_count,first_sample_index,flags,ttl_present\n"
            "0,0,2,100,0,1\n1,2,1,200,3,0\n", encoding="utf-8")

    def test_values_units_shape_and_gap(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.fixture(root)
            data = read_neurox(root)
            self.assertEqual(data["channels"], ["B-09", "C-20"])
            self.assertEqual(data["adc"].shape, (3, 2))
            self.assertIsInstance(data["adc"], np.memmap)
            uv = (data["adc"].astype(float) - 32768) * .195
            np.testing.assert_allclose(uv[1], [-6389.76, 6389.565])
            np.testing.assert_array_equal(data["sample_index"], [100, 101, 200])
            self.assertEqual(data["blocks"][1]["flags"], 3)
            for key in ["adc", "sample_index", "ttl_in"]:
                data[key]._mmap.close()

    def test_incomplete_and_truncated_exports_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.fixture(root)
            (root / "INCOMPLETE.txt").touch()
            with self.assertRaisesRegex(ValueError, "incomplete"):
                read_neurox(root)
            (root / "INCOMPLETE.txt").unlink()
            (root / "samples.u16le").write_bytes(b"\0\0")
            with self.assertRaisesRegex(ValueError, "file size"):
                read_neurox(root)

    @unittest.skipUnless(os.environ.get("NEUROX_EXPORT_DIR"), "no native fixture supplied")
    def test_native_export_round_trip(self):
        data = read_neurox(os.environ["NEUROX_EXPORT_DIR"])
        self.assertEqual(data["adc"].shape, (4, 64))
        self.assertEqual(data["channels"][0], "B-00")
        self.assertEqual(data["channels"][32], "C-00")
        np.testing.assert_array_equal(data["sample_index"], [1000, 1001, 1100, 1101])
        self.assertEqual(int(data["adc"][0, 1]), 32769)
        for key in ["adc", "sample_index", "ttl_in"]:
            data[key]._mmap.close()


if __name__ == "__main__":
    unittest.main()
