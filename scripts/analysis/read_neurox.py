"""Read a NeuroX analysis export. Requires NumPy, not an Intan RHS reader."""
import csv
import json
from pathlib import Path

import numpy as np


def read_neurox(directory):
    """Return memory-mapped raw ADC samples (rows x channels) and metadata.

    Convert only the channels/intervals needed: (adc.astype(float) - 32768) * .195.
    Sample indices preserve missing intervals; never filter across a gap/flag.
    """
    root = Path(directory)
    if (root / "INCOMPLETE.txt").exists():
        raise ValueError("Export is incomplete")
    with (root / "parameters.json").open(encoding="utf-8") as stream:
        metadata = json.load(stream)
    if metadata.get("schemaVersion") != 1 or not metadata.get("complete"):
        raise ValueError("Unsupported or incomplete export")
    rows, channels = metadata["samplesPerChannel"], metadata["channelCount"]
    for name, size in [("samples.u16le", rows * channels * 2),
                       ("sample_index.u64le", rows * 8), ("ttl_in.u16le", rows * 2)]:
        if (root / name).stat().st_size != size:
            raise ValueError("Unexpected file size: " + name)
    with (root / "blocks.csv").open(newline="", encoding="utf-8") as stream:
        blocks = [{key: int(value) for key, value in row.items()}
                  for row in csv.DictReader(stream)]
    return {
        "adc": np.memmap(root / "samples.u16le", dtype="<u2", mode="r", shape=(rows, channels)),
        "sample_index": np.memmap(root / "sample_index.u64le", dtype="<u8", mode="r", shape=(rows,)),
        "ttl_in": np.memmap(root / "ttl_in.u16le", dtype="<u2", mode="r", shape=(rows,)),
        "channels": [channel["label"] for channel in metadata["channels"]],
        "blocks": blocks,
        "parameters": metadata,
    }


if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser()
    parser.add_argument("directory")
    args = parser.parse_args()
    data = read_neurox(args.directory)
    print(data["adc"].shape, data["parameters"]["sampleRateHz"], "Hz")
    print(", ".join(data["channels"]))
