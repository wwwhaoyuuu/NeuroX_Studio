use crate::recording::read_configuration;
use rhs_pipeline::Source;
use rhs_storage::FileReplaySource;
use serde::Serialize;
use serde_json::json;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisExport {
    pub directory: String,
    pub samples_per_channel: u64,
    pub channel_count: usize,
    pub block_count: u64,
    pub csv: bool,
}

#[tauri::command]
pub async fn export_recording_analysis(
    path: String,
    directory: String,
    include_csv: bool,
) -> Result<AnalysisExport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        export_recording(Path::new(&path), Path::new(&directory), include_csv)
    })
    .await
    .map_err(|error| format!("analysis export task failed: {error}"))?
}

fn create_file(directory: &Path, name: &str) -> Result<BufWriter<File>, String> {
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(directory.join(name))
        .map(|file| BufWriter::with_capacity(256 * 1024, file))
        .map_err(|error| format!("create {name}: {error}"))
}

// The storage reader validates block checksums, shape, continuity and footer.
// Keep conversion streaming so long recordings never become one in-memory array.
pub fn export_recording(
    path: &Path,
    directory: &Path,
    include_csv: bool,
) -> Result<AnalysisExport, String> {
    let mut source = FileReplaySource::open(path).map_err(|error| error.to_string())?;
    let info = source.info().clone();
    if !info.complete {
        return Err("Finish recording before exporting; the source is not marked complete".into());
    }
    let configuration = read_configuration(&info.metadata)?;
    if let Some(config) = &configuration {
        if config["channelCount"].as_u64() != Some(info.channels.len() as u64)
            || config["sampleRateHz"].as_u64() != Some(u64::from(info.sample_rate_hz))
        {
            return Err("Recorded configuration does not match the data header".into());
        }
    }
    // Refuse existing destinations, including the original recording directory.
    fs::create_dir(directory).map_err(|error| format!("create a new export directory: {error}"))?;
    fs::write(
        directory.join("INCOMPLETE.txt"),
        b"Export is incomplete. Do not analyze this directory until this marker is removed.\n",
    )
    .map_err(|error| error.to_string())?;
    let mut convert = || -> Result<AnalysisExport, String> {
        let mut samples = create_file(directory, "samples.u16le")?;
        let mut indices = create_file(directory, "sample_index.u64le")?;
        let mut ttl = create_file(directory, "ttl_in.u16le")?;
        let mut blocks = create_file(directory, "blocks.csv")?;
        let mut csv = include_csv
            .then(|| create_file(directory, "samples.csv"))
            .transpose()?;
        let channels: Vec<_> = info
            .channels
            .iter()
            .enumerate()
            .map(|(index, channel)| {
                let port = char::from(b'A' + channel.stream / 2);
                let local = usize::from(channel.stream % 2) * 16 + usize::from(channel.channel);
                json!({"columnIndex": index, "label": format!("{port}-{local:02}"),
                "stream": channel.stream, "chipChannel": channel.channel,
                "port": port.to_string(), "portChannel": local})
            })
            .collect();
        writeln!(
            blocks,
            "sequence,row_start,sample_count,first_sample_index,flags,ttl_present"
        )
        .map_err(|error| error.to_string())?;
        if let Some(csv) = csv.as_mut() {
            write!(csv, "sample_index,time_seconds,ttl_in,flags")
                .map_err(|error| error.to_string())?;
            for channel in &channels {
                write!(csv, ",{}_uV", channel["label"].as_str().unwrap())
                    .map_err(|error| error.to_string())?;
            }
            writeln!(csv).map_err(|error| error.to_string())?;
        }
        let mut rows = 0_u64;
        let mut block_count = 0_u64;
        let mut origin = None;
        let mut last_index = 0;
        while let Some(block) = source.next_block().map_err(|error| error.to_string())? {
            let first = *origin.get_or_insert(block.first_sample_index);
            writeln!(
                blocks,
                "{},{},{},{},{},{}",
                block.sequence,
                rows,
                block.samples_per_channel,
                block.first_sample_index,
                block.flags.bits(),
                u8::from(!block.ttl_in.is_empty())
            )
            .map_err(|error| error.to_string())?;
            let mut bytes = Vec::with_capacity(block.samples.len() * 2);
            for word in &block.samples {
                bytes.extend_from_slice(&word.to_le_bytes());
            }
            samples
                .write_all(&bytes)
                .map_err(|error| error.to_string())?;
            for offset in 0..block.samples_per_channel as usize {
                let index = block.first_sample_index + offset as u64;
                indices
                    .write_all(&index.to_le_bytes())
                    .map_err(|error| error.to_string())?;
                let ttl_value = block.ttl_in.get(offset).copied().unwrap_or(0);
                ttl.write_all(&ttl_value.to_le_bytes())
                    .map_err(|error| error.to_string())?;
                if let Some(csv) = csv.as_mut() {
                    let seconds =
                        (index as i128 - first as i128) as f64 / f64::from(info.sample_rate_hz);
                    write!(
                        csv,
                        "{index},{seconds:.9},{ttl_value},{}",
                        block.flags.bits()
                    )
                    .map_err(|error| error.to_string())?;
                    for word in &block.samples
                        [offset * info.channels.len()..(offset + 1) * info.channels.len()]
                    {
                        let uv = (f64::from(*word) - 32768.0) * 0.195;
                        write!(csv, ",{uv:.3}").map_err(|error| error.to_string())?;
                    }
                    writeln!(csv).map_err(|error| error.to_string())?;
                }
            }
            rows += u64::from(block.samples_per_channel);
            block_count += 1;
            last_index = block.last_sample_index();
        }
        if rows == 0 {
            return Err("Recording has no samples".into());
        }
        for writer in [&mut samples, &mut indices, &mut ttl, &mut blocks] {
            writer
                .flush()
                .and_then(|()| writer.get_ref().sync_all())
                .map_err(|error| error.to_string())?;
        }
        if let Some(csv) = csv.as_mut() {
            csv.flush()
                .and_then(|()| csv.get_ref().sync_all())
                .map_err(|error| error.to_string())?;
        }
        let metadata = json!({
            "schemaVersion": 1, "complete": true, "sourceComplete": info.complete,
            "sourceFile": path.to_string_lossy(), "sourceId": info.source_id,
            "createdUnixSeconds": info.created_unix_seconds,
            "sampleRateHz": info.sample_rate_hz, "channelCount": info.channels.len(),
            "samplesPerChannel": rows, "blockCount": block_count,
            "firstSampleIndex": origin, "lastSampleIndex": last_index,
            "sampleLayout": "sample-major-interleaved", "sampleDtype": "<u2",
            "adcZero": 32768, "microvoltsPerAdc": 0.195,
            "signal": "raw-before-software-processing", "channels": channels,
            "configuration": configuration, "sourceMetadata": info.metadata,
            "csvIncluded": include_csv,
            "blockFlags": {"discontinuity": 1, "stimulationArtifact": 2, "deviceWarning": 4},
            "timeReference": "(sample_index - firstSampleIndex) / sampleRateHz; gaps are not filled",
            "ttlMissing": "A zero is written when TTL is absent; consult ttl_present in blocks.csv"
        });
        let mut parameters = create_file(directory, "parameters.json")?;
        serde_json::to_writer_pretty(&mut parameters, &metadata)
            .map_err(|error| error.to_string())?;
        parameters
            .flush()
            .and_then(|()| parameters.get_ref().sync_all())
            .map_err(|error| error.to_string())?;
        for (name, text) in [
            (
                "read_neurox.py",
                include_str!("../../../../scripts/analysis/read_neurox.py"),
            ),
            (
                "read_neurox.m",
                include_str!("../../../../scripts/analysis/read_neurox.m"),
            ),
            (
                "README.md",
                include_str!("../../../../scripts/analysis/README.md"),
            ),
        ] {
            let mut writer = create_file(directory, name)?;
            writer
                .write_all(text.as_bytes())
                .and_then(|()| writer.flush())
                .and_then(|()| writer.get_ref().sync_all())
                .map_err(|error| error.to_string())?;
        }
        fs::remove_file(directory.join("INCOMPLETE.txt")).map_err(|error| error.to_string())?;
        Ok(AnalysisExport {
            directory: directory.to_string_lossy().into_owned(),
            samples_per_channel: rows,
            channel_count: info.channels.len(),
            block_count,
            csv: include_csv,
        })
    };
    convert()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording::configuration_entries;
    use rhs_core::{ChannelId, SampleBlock, SampleFlags};
    use rhs_pipeline::Sink;
    use rhs_storage::RawRecorderSink;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn directory() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "neurox-export-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        root
    }

    fn recording(root: &Path, channels: usize, finalize: bool, config: bool) -> PathBuf {
        let path = root.join("input.rhsraw");
        let metadata = if config {
            configuration_entries(&json!({"schemaVersion": 1, "channelCount": channels,
                "sampleRateHz": 30000, "workspace": {"session": {"notes": "测试"}}}))
            .unwrap()
        } else {
            vec![]
        };
        let mut sink = RawRecorderSink::create_with_metadata(&path, metadata).unwrap();
        for (sequence, index, flags, ttl) in [(0, 1000, 0, vec![1, 2]), (1, 1100, 3, vec![])] {
            let block = SampleBlock::new(
                "export-test",
                sequence,
                index,
                30000,
                (0..channels)
                    .map(|column| ChannelId {
                        stream: 2 + (column / 16) as u8,
                        channel: (column % 16) as u8,
                    })
                    .collect(),
                2,
                (0..channels * 2)
                    .map(|value| 32768 + value as u16)
                    .collect(),
                ttl,
                SampleFlags::from_bits(flags),
            )
            .unwrap();
            sink.consume(&block).unwrap();
        }
        if finalize {
            sink.finalize().unwrap();
        }
        path
    }

    #[test]
    fn full_rate_export_preserves_64_physical_channels_time_gaps_ttl_and_parameters() {
        let root = directory();
        let path = recording(&root, 64, true, true);
        let before = fs::read(&path).unwrap();
        let destination = root.join("analysis");
        let result = export_recording(&path, &destination, true).unwrap();
        assert_eq!(
            (
                result.samples_per_channel,
                result.channel_count,
                result.block_count
            ),
            (4, 64, 2)
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!destination.join("INCOMPLETE.txt").exists());
        let p: serde_json::Value =
            serde_json::from_slice(&fs::read(destination.join("parameters.json")).unwrap())
                .unwrap();
        assert_eq!(p["channels"][0]["label"], "B-00");
        assert_eq!(p["channels"][31]["label"], "B-31");
        assert_eq!(p["channels"][32]["label"], "C-00");
        assert_eq!(p["channels"][63]["label"], "C-31");
        assert_eq!(p["configuration"]["workspace"]["session"]["notes"], "测试");
        let words = fs::read(destination.join("samples.u16le")).unwrap();
        assert_eq!(words.len(), 4 * 64 * 2);
        assert_eq!(u16::from_le_bytes([words[2], words[3]]), 32769);
        let indices: Vec<_> = fs::read(destination.join("sample_index.u64le"))
            .unwrap()
            .chunks_exact(8)
            .map(|bytes| u64::from_le_bytes(bytes.try_into().unwrap()))
            .collect();
        assert_eq!(indices, vec![1000, 1001, 1100, 1101]);
        assert_eq!(
            fs::read(destination.join("ttl_in.u16le")).unwrap(),
            vec![1, 0, 2, 0, 0, 0, 0, 0]
        );
        let csv = fs::read_to_string(destination.join("samples.csv")).unwrap();
        assert_eq!(csv.lines().count(), 5);
        assert!(csv.lines().next().unwrap().ends_with("C-31_uV"));
        assert!(csv
            .lines()
            .nth(1)
            .unwrap()
            .starts_with("1000,0.000000000,1,0,0.000,0.195"));
        assert!(csv
            .lines()
            .nth(3)
            .unwrap()
            .starts_with("1100,0.003333333,0,3,"));
        let blocks = fs::read_to_string(destination.join("blocks.csv")).unwrap();
        assert!(blocks.ends_with("1,2,2,1100,3,0\n"));
        assert!(destination.join("read_neurox.py").exists());
        assert!(destination.join("read_neurox.m").exists());
        assert!(export_recording(&path, &destination, true).is_err());
        assert_eq!(fs::read(destination.join("samples.u16le")).unwrap(), words);
    }

    #[test]
    fn legacy_binary_only_and_single_chip_exports_work() {
        let root = directory();
        let path = recording(&root, 16, true, false);
        let destination = root.join("analysis");
        export_recording(&path, &destination, false).unwrap();
        assert!(!destination.join("samples.csv").exists());
        let p: serde_json::Value =
            serde_json::from_slice(&fs::read(destination.join("parameters.json")).unwrap())
                .unwrap();
        assert!(p["configuration"].is_null());
        assert_eq!(p["channelCount"], 16);
    }

    #[test]
    fn incomplete_and_corrupt_sources_never_look_like_successful_exports() {
        let root = directory();
        let incomplete = recording(&root, 16, false, false);
        let destination = root.join("analysis");
        assert!(export_recording(&incomplete, &destination, true).is_err());
        assert!(!destination.exists());
        let root = directory();
        let corrupt = recording(&root, 16, true, false);
        let mut bytes = fs::read(&corrupt).unwrap();
        // Change a payload byte, not the file header; CRC must reject it.
        let marker = bytes.windows(4).position(|bytes| bytes == b"BLK1").unwrap();
        bytes[marker + 12 + 20] ^= 1;
        fs::write(&corrupt, bytes).unwrap();
        let destination = root.join("analysis");
        assert!(export_recording(&corrupt, &destination, true)
            .unwrap_err()
            .contains("checksum"));
        assert!(destination.join("INCOMPLETE.txt").exists());
        assert!(!destination.join("parameters.json").exists());
    }
}
