"""Dependency-free format tests; optional integration with the official reader."""
from array import array
import contextlib
import hashlib
import importlib.util
import io
import json
import math
import os
from pathlib import Path
import struct
import sys
import tempfile
import types
import unittest
from unittest.mock import patch
import zlib

from rhsraw_to_rhs import convert, FormatError, little_bytes


def make_raw(path, nc=16, counts=(73, 183), indices=None, ttl_modes=None,
             complete=True, channels=None):
    channels = channels or [(2 + i // 16, i % 16) for i in range(nc)]
    indices = indices or [1000 + sum(counts[:i]) for i in range(len(counts))]
    ttl_modes = ttl_modes or [True] * len(counts)
    source = b"independent-format-test"
    key, value = b"session_name", "test-电极".encode("utf-8")
    with path.open("wb") as file:
        file.write(b"RHSRAW1\0" + struct.pack("<HBBQIHHH", 1, int(complete), 0, 1,
                                              30000, nc, len(source), 1) + source)
        for address in channels:
            file.write(bytes(address))
        file.write(struct.pack("<HH", len(key), len(value)) + key + value)
        row_start = 0
        for sequence, (n, index, has_ttl) in enumerate(zip(counts, indices, ttl_modes)):
            flags = 3 if sequence and index != indices[sequence - 1] + counts[sequence - 1] else 0
            adc = array("H", [((row_start + row) * nc + column) % 65536
                              for row in range(n) for column in range(nc)])
            ttl = array("H", [(row_start + row) % 65536 for row in range(n)]) if has_ttl else array("H")
            payload = struct.pack("<QQIIII", sequence, index, n, flags, len(adc), len(ttl))
            payload += little_bytes(adc) + little_bytes(ttl)
            file.write(b"BLK1" + struct.pack("<II", len(payload), zlib.crc32(payload)) + payload)
            row_start += n
        file.write(b"END1" + struct.pack("<QQ", len(counts), sum(counts)))
    return channels


def read_rhs(path):
    """Separate specification-based parser, independent of the converter writer."""
    with path.open("rb") as file:
        def get(fmt):
            return struct.unpack("<" + fmt, file.read(struct.calcsize("<" + fmt)))

        def string():
            n, = get("I")
            return file.read(n).decode("utf-16-le")

        magic, major, minor, rate = get("Ihhf")
        assert (magic, major, minor) == (0xD69127AC, 1, 0)
        freq = get("h8fh2fhh3f")
        notes = [string() for _ in range(3)]
        dc, board = get("hh")
        reference = string()
        groups, = get("h")
        channels, digital = [], []
        for _ in range(groups):
            group_name, prefix = string(), string()
            enabled, count, amplifiers = get("hhh")
            for _ in range(count):
                native, custom = string(), string()
                fields = get("hhhhhHh4h2f")
                channel = {"native": native, "custom": custom, "stream": fields[6],
                           "chip": fields[4], "kind": fields[2]}
                (channels if fields[2] == 0 else digital).append(channel)
        times, samples, ttl, stim = [], [[] for _ in channels], [], []
        block_size = 128 * (4 + 4 * len(channels) + (2 if digital else 0))
        data = file.read()
        assert len(data) % block_size == 0
        cursor = io.BytesIO(data)
        for _ in range(len(data) // block_size):
            times.extend(struct.unpack("<128i", cursor.read(512)))
            for values in samples:
                values.extend(struct.unpack("<128H", cursor.read(256)))
            stim.extend(cursor.read(256 * len(channels)))
            if digital:
                ttl.extend(struct.unpack("<128H", cursor.read(256)))
        return {"rate": rate, "channels": channels, "digital": digital,
                "times": times, "samples": samples, "ttl": ttl, "stim": stim,
                "notes": notes, "dc": dc, "board": board, "freq": freq}


class ConverterTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.raw, self.rhs = self.root / "recording.rhsraw", self.root / "recording.rhs"

    def tearDown(self):
        self.temp.cleanup()

    def test_16_64_120_channel_data_units_and_physical_reordering(self):
        for nc in (16, 64, 120):
            with self.subTest(channels=nc):
                raw, rhs = self.root / f"test{nc}.rhsraw", self.root / f"test{nc}.rhs"
                # Include all 8 possible physical streams for the 120-channel test.
                addresses = [(i // 16 if nc == 120 else 2 + i // 16, i % 16)
                             for i in reversed(range(nc))]
                make_raw(raw, nc, channels=addresses)
                original = raw.read_bytes()
                report = convert(raw, rhs)
                actual = read_rhs(rhs)
                self.assertEqual(raw.read_bytes(), original)
                self.assertEqual(report["sourceSHA256"], hashlib.sha256(original).hexdigest())
                self.assertEqual(actual["rate"], 30000)
                self.assertEqual(actual["times"], list(range(256)))
                self.assertEqual(len(actual["channels"]), nc)
                for column, mapping in enumerate(report["channels"]):
                    expected = [(row * nc + mapping["sourceColumn"]) % 65536 for row in range(256)]
                    self.assertEqual(actual["samples"][column], expected)
                    self.assertEqual(actual["channels"][column]["native"], mapping["nativeName"])
                self.assertEqual(actual["ttl"], list(range(256)))
                self.assertTrue(math.isnan(actual["freq"][-3]))
                self.assertIn("UNKNOWN", actual["notes"][1])
                self.assertFalse(any(actual["stim"]))
                self.assertEqual(actual["dc"], 0)

    def test_gaps_large_original_indices_and_block_flags(self):
        origin = 2**63 + 1000
        make_raw(self.raw, counts=(128, 128), indices=[origin, origin + 512])
        report = convert(self.raw, self.rhs)
        result = read_rhs(self.rhs)
        self.assertEqual(result["times"], list(range(128)) + list(range(512, 640)))
        self.assertEqual(report["timestampOriginSampleIndex"], origin)
        blocks = [json.loads(row) for row in Path(str(self.rhs) + ".blocks.jsonl").read_text().splitlines()]
        self.assertEqual(blocks[1]["flags"], 3)
        self.assertEqual(blocks[1]["firstSampleIndex"], origin + 512)

    def test_missing_or_partial_ttl_is_omitted_not_fabricated(self):
        for modes in ([False, False], [True, False]):
            raw, rhs = self.root / f"ttl{modes[0]}.rhsraw", self.root / f"ttl{modes[0]}.rhs"
            make_raw(raw, ttl_modes=modes)
            self.assertFalse(convert(raw, rhs)["ttlExported"])
            self.assertEqual(read_rhs(rhs)["digital"], [])

    def test_tail_requires_explicit_padding_and_retains_valid_count(self):
        make_raw(self.raw, counts=(129,))
        with self.assertRaisesRegex(FormatError, "128-sample"):
            convert(self.raw, self.rhs)
        self.assertFalse(self.rhs.exists())
        report = convert(self.raw, self.rhs, pad_tail=True)
        self.assertEqual(report["validSamplesPerChannel"], 129)
        self.assertEqual(report["paddedSamplesPerChannel"], 127)
        result = read_rhs(self.rhs)
        self.assertEqual(result["samples"][0][129:], [128 * 16] * 127)
        self.assertEqual(len(result["times"]), 256)

    def test_corruption_footer_and_truncation_fail_before_creating_output(self):
        make_raw(self.raw)
        original = self.raw.read_bytes()
        for kind in ("crc", "footer", "truncated", "trailing"):
            data = bytearray(original)
            if kind == "crc":
                data[data.index(b"BLK1") + 12 + 20] ^= 1
            elif kind == "footer":
                data[-1] ^= 1
            elif kind == "truncated":
                del data[-5:]
            else:
                data.extend(b"extra")
            self.raw.write_bytes(data)
            with self.subTest(kind=kind), self.assertRaises(FormatError):
                convert(self.raw, self.rhs)
            self.assertFalse(self.rhs.exists())

    def test_unfinished_and_duplicate_channels_rejected(self):
        make_raw(self.raw, complete=False)
        with self.assertRaisesRegex(FormatError, "unfinished"):
            convert(self.raw, self.rhs)
        make_raw(self.raw, channels=[(2, 0)] * 16)
        with self.assertRaisesRegex(FormatError, "Duplicate"):
            convert(self.raw, self.rhs)

    def test_timestamp_overflow_rejected_not_wrapped(self):
        make_raw(self.raw, counts=(128, 128), indices=[1000, 2**31 + 1000])
        with self.assertRaisesRegex(FormatError, "int32"):
            convert(self.raw, self.rhs)

    def test_existing_output_preserved_and_failed_publish_is_marked(self):
        make_raw(self.raw)
        self.rhs.write_bytes(b"original")
        with self.assertRaises(FileExistsError):
            convert(self.raw, self.rhs)
        self.assertEqual(self.rhs.read_bytes(), b"original")
        other = self.root / "publish-failure.rhs"
        with patch("rhsraw_to_rhs.publish", side_effect=OSError("simulated full disk")):
            with self.assertRaises(OSError):
                convert(self.raw, other)
        self.assertFalse(other.exists())
        self.assertTrue(Path(str(other) + ".INCOMPLETE").exists())
        self.assertTrue(Path(str(other) + ".part").exists())

    @unittest.skipUnless(os.environ.get("INTAN_RHS_READER"), "official reader unavailable")
    def test_official_intan_reader(self):
        import numpy as np
        spec = importlib.util.spec_from_file_location("official_rhs", os.environ["INTAN_RHS_READER"])
        official = importlib.util.module_from_spec(spec)
        # The official notebook helper imports plotting even for file-only reads.
        # Isolate that unused optional dependency; no parsing function is mocked.
        with contextlib.ExitStack() as stack:
            if importlib.util.find_spec("matplotlib") is None:
                plotting = types.ModuleType("matplotlib.pyplot")
                package = types.ModuleType("matplotlib")
                package.pyplot = plotting
                stack.enter_context(patch.dict(sys.modules, {"matplotlib": package, "matplotlib.pyplot": plotting}))
            spec.loader.exec_module(official)
        for nc in (16, 64, 120):
            raw, rhs = self.root / f"official{nc}.rhsraw", self.root / f"official{nc}.rhs"
            addresses = [(i // 16, i % 16) for i in range(nc)]
            make_raw(raw, nc, channels=addresses, counts=(128, 128), indices=[1000, 1512])
            convert(raw, rhs)
            with contextlib.redirect_stdout(io.StringIO()):
                result, present = official.load_file(str(rhs))
            self.assertTrue(present)
            self.assertEqual(result["amplifier_data"].shape, (nc, 256))
            self.assertEqual(result["amplifier_channels"][-1]["board_stream"], addresses[-1][0])
            np.testing.assert_allclose(result["amplifier_data"][0], (np.arange(256) * nc - 32768) * .195)
            self.assertTrue(np.isnan(result["stim_data"]).all())
            self.assertEqual(len(result["board_dig_in_channels"]), 16)
            self.assertAlmostEqual(result["t"][128], 512 / 30000)


if __name__ == "__main__":
    unittest.main()
