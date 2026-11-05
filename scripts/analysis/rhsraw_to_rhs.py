"""Convert NeuroX RHSRAW v1 to traditional Intan RHS v1.0 for analysis.

Python 3.9+, standard library only. Not a driver; never opens hardware.
"""
import argparse
from array import array
import hashlib
import json
import os
from pathlib import Path
import struct
import sys
import zlib

FORMAT_URL = "https://intantech.com/files/Intan_RHS2000_data_file_formats.pdf"
MAX_PAYLOAD = 512 * 1024 * 1024
UNKNOWN = float("nan")


class FormatError(ValueError):
    pass


def words(data, code="H"):
    result = array(code)
    result.frombytes(data)
    if sys.byteorder != "little":
        result.byteswap()
    return result


def little_bytes(values):
    if sys.byteorder == "little":
        return values.tobytes()
    result = array(values.typecode, values)
    result.byteswap()
    return result.tobytes()


class RawReader:
    def __init__(self, file):
        self.file = file
        self.digest = hashlib.sha256()
        self.header = self.read_header()

    def read(self, count):
        data = self.file.read(count)
        if len(data) != count:
            raise FormatError("Truncated RHSRAW file")
        self.digest.update(data)
        return data

    def unpack(self, fmt):
        return struct.unpack("<" + fmt, self.read(struct.calcsize("<" + fmt)))

    def read_header(self):
        if self.read(8) != b"RHSRAW1\0":
            raise FormatError("Not a NeuroX RHSRAW v1 file")
        version, complete, reserved, created, rate, count, source_len, metadata_count = self.unpack("HBBQIHHH")
        if version != 1 or complete != 1:
            raise FormatError("Unsupported or unfinished recording; finish recording first")
        if not rate or not 1 <= count <= 128 or not source_len:
            raise FormatError("Invalid sample rate, channel count or source ID")
        if struct.unpack("<f", struct.pack("<f", rate))[0] != rate:
            raise FormatError("Sample rate cannot be represented exactly in the RHS header")
        try:
            source_id = self.read(source_len).decode("utf-8")
            channels = [self.unpack("BB") for _ in range(count)]
            if len(set(channels)) != count or any(s > 7 or c > 15 for s, c in channels):
                raise FormatError("Duplicate or invalid physical RHS2116 channel addresses")
            metadata, metadata_bytes = [], 0
            for _ in range(metadata_count):
                key_len, value_len = self.unpack("HH")
                metadata_bytes += key_len + value_len
                if not key_len or metadata_bytes > 16 * 1024 * 1024:
                    raise FormatError("Invalid or excessive recording metadata")
                metadata.append([self.read(key_len).decode("utf-8"), self.read(value_len).decode("utf-8")])
        except UnicodeError as error:
            raise FormatError("Invalid UTF-8 recording metadata") from error
        return {"version": version, "createdUnixSeconds": created, "sampleRateHz": rate,
                "sourceId": source_id, "channels": channels, "metadata": metadata,
                "reservedHeaderByte": reserved}

    def blocks(self):
        rows, block_count, expected_sequence, expected_index = 0, 0, None, None
        channel_count = len(self.header["channels"])
        while True:
            marker = self.read(4)
            if marker == b"END1":
                footer_blocks, footer_rows = self.unpack("QQ")
                if (footer_blocks, footer_rows) != (block_count, rows):
                    raise FormatError("RHSRAW footer counts do not match the data")
                if self.file.read(1):
                    raise FormatError("Unexpected bytes after RHSRAW footer")
                return
            if marker != b"BLK1":
                raise FormatError("Invalid RHSRAW block marker")
            size, crc = self.unpack("II")
            if not 32 <= size <= MAX_PAYLOAD:
                raise FormatError("Invalid RHSRAW payload length")
            payload = self.read(size)
            if zlib.crc32(payload) & 0xFFFFFFFF != crc:
                raise FormatError("RHSRAW checksum mismatch")
            sequence, index, n, flags, sample_count, ttl_count = struct.unpack_from("<QQIIII", payload)
            if not n or sample_count != n * channel_count or ttl_count not in (0, n):
                raise FormatError("RHSRAW sample dimensions do not match the header")
            if size != 32 + 2 * (sample_count + ttl_count) or index + n - 1 > 2**64 - 1:
                raise FormatError("Invalid RHSRAW payload shape or sample index range")
            if expected_sequence is not None and sequence != expected_sequence:
                raise FormatError("RHSRAW block sequence mismatch")
            if expected_index is not None and index != expected_index and not flags & 1:
                raise FormatError("RHSRAW time discontinuity without a discontinuity flag")
            expected_sequence, expected_index = sequence + 1, index + n
            yield {"sequence": sequence, "firstSampleIndex": index, "sampleCount": n,
                   "flags": flags, "rowStart": rows, "ttlAvailable": ttl_count != 0,
                   "adc": words(payload[32:32 + sample_count * 2]),
                   "ttl": words(payload[32 + sample_count * 2:])}
            rows += n
            block_count += 1


def scan(reader):
    rows, blocks, ttl_rows, origin, low, high, flags = 0, 0, 0, None, None, None, 0
    for block in reader.blocks():
        index, n = block["firstSampleIndex"], block["sampleCount"]
        if origin is None:
            origin = index
        low = index if low is None else min(low, index)
        high = index + n - 1 if high is None else max(high, index + n - 1)
        rows += n
        blocks += 1
        ttl_rows += n if block["ttlAvailable"] else 0
        flags |= block["flags"]
    if not rows:
        raise FormatError("Recording has no samples")
    if low - origin < -(2**31) or high - origin > 2**31 - 1:
        raise FormatError("Relative timestamps exceed RHS int32 range; split this recording first")
    return {"rows": rows, "blocks": blocks, "ttlRows": ttl_rows,
            "origin": origin, "lastTimestamp": high - origin, "flags": flags,
            "sha256": reader.digest.hexdigest()}


def qstring(file, value):
    encoded = value.encode("utf-16-le")
    file.write(struct.pack("<I", len(encoded)))
    file.write(encoded)


def write_channel(file, name, custom, order, kind, chip, stream):
    qstring(file, name)
    qstring(file, custom)
    file.write(struct.pack("<hhhhhHh", order, order, kind, 1, chip, stream, stream))
    file.write(struct.pack("<hhhhff", 0, 0, 0, 0, UNKNOWN, UNKNOWN))


def write_header(file, header, order, include_ttl, padded):
    file.write(struct.pack("<Ihhf", 0xD69127AC, 1, 0, header["sampleRateHz"]))
    # Hardware floats are unknown. Discrete mandatory fields are placeholders.
    file.write(struct.pack("<h8fh2fhh3f", 0, *([UNKNOWN] * 8), 0,
                           UNKNOWN, UNKNOWN, 0, 0, UNKNOWN, UNKNOWN, UNKNOWN))
    qstring(file, "NeuroX conversion: raw amplifier data and available DIN only; not an Intan acquisition.")
    qstring(file, "Stimulation/compliance/settle/recovery UNKNOWN: mandatory stim words are zero placeholders, current scale is NaN. Zero bits do NOT prove no stimulation.")
    qstring(file, "Unknown analog settings/impedance=NaN; notch=0 (no added filtering). See .rhs.json/.blocks.jsonl. Tail padding: " + str(padded))
    file.write(struct.pack("<hh", 0, 14))  # No DC data; RHS compatibility board mode.
    qstring(file, "n/a")
    ports = sorted({header["channels"][i][0] // 2 for i in order})
    file.write(struct.pack("<h", len(ports) + int(include_ttl)))
    for port in ports:
        letter = chr(65 + port)
        indices = [i for i in order if header["channels"][i][0] // 2 == port]
        qstring(file, "Port " + letter)
        qstring(file, letter)
        file.write(struct.pack("<hhh", 1, len(indices), len(indices)))
        for i in indices:
            stream, chip = header["channels"][i]
            local = (stream % 2) * 16 + chip
            write_channel(file, f"{letter}-{local:03}", f"{letter}-{local:02}", local, 0, chip, stream)
    if include_ttl:
        qstring(file, "Board Digital Inputs")
        qstring(file, "DIN")
        file.write(struct.pack("<hhh", 1, 16, 0))
        for bit in range(16):
            write_channel(file, f"DIN-{bit:02}", f"DIN-{bit:02}", bit, 5, bit, 0)


def publish(part, destination):
    # Never replace a file that appeared while conversion was running.
    if os.name == "nt":
        os.rename(part, destination)
    else:
        os.link(part, destination)
        part.unlink()


def convert(source, destination, pad_tail=False):
    source, destination = Path(source).resolve(), Path(destination).resolve()
    if source == destination or destination.suffix.lower() != ".rhs":
        raise ValueError("Choose a different output path ending in .rhs")
    if not destination.parent.is_dir():
        raise ValueError("The output parent directory must already exist")
    report_path = Path(str(destination) + ".json")
    blocks_path = Path(str(destination) + ".blocks.jsonl")
    marker = Path(str(destination) + ".INCOMPLETE")
    finals = [destination, report_path, blocks_path]
    parts = [Path(str(p) + ".part") for p in finals]
    for p in finals + parts + [marker]:
        if p.exists() or p.is_symlink():
            raise FileExistsError(f"Refusing to overwrite: {p}")
    with source.open("rb") as raw:
        first_reader = RawReader(raw)
        header = first_reader.header
        summary = scan(first_reader)
        remainder = summary["rows"] % 128
        padded = (128 - remainder) % 128
        if padded and not pad_tail:
            raise FormatError(f"Source ends with {remainder} samples outside a 128-sample RHS block. No output written. Use --pad-tail only if explicit synthetic tail padding is acceptable.")
        if summary["lastTimestamp"] + padded > 2**31 - 1:
            raise FormatError("Padded timestamps exceed RHS int32 range")
        include_ttl = summary["ttlRows"] == summary["rows"]
        order = sorted(range(len(header["channels"])), key=lambda i: header["channels"][i])
        channels = []
        for target, i in enumerate(order):
            stream, chip = header["channels"][i]
            local, letter = (stream % 2) * 16 + chip, chr(65 + stream // 2)
            channels.append({"rhsColumn": target, "sourceColumn": i, "stream": stream,
                             "chipChannel": chip, "nativeName": f"{letter}-{local:03}"})
        report = {"schemaVersion": 1, "complete": True, "format": "Intan traditional RHS v1.0",
                  "formatSpecification": FORMAT_URL, "source": str(source), "sourceHeader": header,
                  "sourceSHA256": summary["sha256"], "validSamplesPerChannel": summary["rows"],
                  "paddedSamplesPerChannel": padded, "padding": "repeat last ADC/TTL; extrapolate timestamps" if padded else None,
                  "sampleRateHz": header["sampleRateHz"], "timestampOriginSampleIndex": summary["origin"],
                  "timestamps": "original sample index minus origin; gaps preserved, no interpolation",
                  "channels": channels, "ttlExported": include_ttl,
                  "ttlOmissionReason": None if include_ttl else "TTL absent in at least one source block; no invented digital-low samples",
                  "unavailable": ["per-sample stimulation current and compliance/settle/recovery status",
                                  "DC amplifier and board ADC/DAC/DOUT waveforms",
                                  "actual hardware filter/impedance/reference settings"],
                  "stimEncoding": "mandatory zero words + NaN current scaling; Boolean stim status outputs are placeholders, not measurements",
                  "blockFlags": {"discontinuity": 1, "stimulationArtifact": 2, "deviceWarning": 4},
                  "softwareFilteringApplied": False, "sourceBlockCount": summary["blocks"]}
        with marker.open("x", encoding="utf-8") as status:
            status.write("Conversion in progress or failed. Do not use output until this marker is removed.\n")
        raw.seek(0)
        second_reader = RawReader(raw)
        nc = len(order)
        if second_reader.header != header:
            raise FormatError("Source header changed during conversion")
        with parts[0].open("xb") as rhs, parts[2].open("x", encoding="utf-8", newline="\n") as index_file:
            write_header(rhs, header, order, include_ttl, padded)
            adc, timestamps, ttl = array("H"), array("i"), array("H")

            def flush():
                rhs.write(little_bytes(timestamps))
                for column in order:
                    rhs.write(little_bytes(adc[column::nc]))
                rhs.write(b"\0" * (nc * 128 * 2))
                if include_ttl:
                    rhs.write(little_bytes(ttl))
                del adc[:], timestamps[:], ttl[:]

            for block in second_reader.blocks():
                index_file.write(json.dumps({k: v for k, v in block.items() if k not in ("adc", "ttl")}) + "\n")
                offset = 0
                while offset < block["sampleCount"]:
                    take = min(128 - len(timestamps), block["sampleCount"] - offset)
                    start = block["firstSampleIndex"] - summary["origin"] + offset
                    adc.extend(block["adc"][offset * nc:(offset + take) * nc])
                    timestamps.extend(range(start, start + take))
                    if include_ttl:
                        ttl.extend(block["ttl"][offset:offset + take])
                    offset += take
                    if len(timestamps) == 128:
                        flush()
            if timestamps:
                last_row, last_index = adc[-nc:], timestamps[-1]
                last_ttl = ttl[-1] if include_ttl else 0
                for i in range(padded):
                    adc.extend(last_row)
                    timestamps.append(last_index + i + 1)
                    if include_ttl:
                        ttl.append(last_ttl)
                flush()
            if second_reader.digest.hexdigest() != summary["sha256"]:
                raise FormatError("Source data changed during conversion")
            for file in (rhs, index_file):
                file.flush()
                os.fsync(file.fileno())
        with parts[1].open("x", encoding="utf-8", newline="\n") as file:
            json.dump(report, file, ensure_ascii=False, indent=2, allow_nan=False)
            file.flush()
            os.fsync(file.fileno())
        for part, final in [(parts[2], finals[2]), (parts[1], finals[1]), (parts[0], finals[0])]:
            publish(part, final)
        marker.unlink()
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", help="completed .rhsraw recording")
    parser.add_argument("-o", "--output", required=True, help="new .rhs path; choose a disk with free space")
    parser.add_argument("--pad-tail", action="store_true", help="explicitly pad final partial block; see report for valid sample count")
    args = parser.parse_args()
    try:
        report = convert(args.input, args.output, args.pad_tail)
    except (OSError, ValueError, OverflowError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 1
    print(f"Created {args.output}: {report['validSamplesPerChannel']} valid samples/channel, {len(report['channels'])} channels")
    print("WARNING: Stimulation state was not recorded; RHS stimulation fields are NOT experimental evidence.")
    if report["paddedSamplesPerChannel"]:
        print(f"WARNING: Exclude the final {report['paddedSamplesPerChannel']} synthetic padded samples from analysis.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
