use rhs_core::{ChannelId, SampleBlock, SampleFlags, SAMPLE_BLOCK_FORMAT_VERSION};
use rhs_pipeline::{PipelineError, Sink, Source, SourceError};
use std::error::Error as StdError;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const FILE_MAGIC: &[u8; 8] = b"RHSRAW1\0";
const FILE_VERSION: u16 = 1;
const COMPLETE_OFFSET: u64 = 10;
const BLOCK_MARKER: &[u8; 4] = b"BLK1";
const FOOTER_MARKER: &[u8; 4] = b"END1";
const MAX_RECORD_BYTES: usize = 512 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingInfo {
    pub version: u16,
    pub complete: bool,
    pub created_unix_seconds: u64,
    pub sample_rate_hz: u32,
    pub source_id: String,
    pub channels: Vec<ChannelId>,
    pub metadata: Vec<(String, String)>,
}

pub struct RawRecorderSink {
    path: PathBuf,
    file: File,
    metadata: Vec<(String, String)>,
    info: Option<RecordingInfo>,
    block_count: u64,
    samples_per_channel: u64,
    next_sequence: Option<u64>,
    next_sample_index: Option<u64>,
    finished: bool,
}

impl RawRecorderSink {
    pub fn create(path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        Self::create_with_metadata(path, Vec::new())
    }

    pub fn create_with_metadata(
        path: impl Into<PathBuf>,
        metadata: Vec<(String, String)>,
    ) -> Result<Self, StorageError> {
        let path = path.into();
        validate_metadata(&metadata)?;
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| StorageError::io("create recording", &path, error))?;
        Ok(Self {
            path,
            file,
            metadata,
            info: None,
            block_count: 0,
            samples_per_channel: 0,
            next_sequence: None,
            next_sample_index: None,
            finished: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn block_count(&self) -> u64 {
        self.block_count
    }

    fn write_header(&mut self, block: &SampleBlock) -> Result<(), StorageError> {
        if block.source_id.len() > u16::MAX as usize {
            return Err(StorageError::new(
                "source_id is too long for recording format",
            ));
        }
        if block.channels.len() > u16::MAX as usize {
            return Err(StorageError::new("too many channels for recording format"));
        }

        let created_unix_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| StorageError::new("system time is before UNIX epoch"))?
            .as_secs();
        let info = RecordingInfo {
            version: FILE_VERSION,
            complete: false,
            created_unix_seconds,
            sample_rate_hz: block.sample_rate_hz,
            source_id: block.source_id.clone(),
            channels: block.channels.clone(),
            metadata: self.metadata.clone(),
        };

        self.file
            .write_all(FILE_MAGIC)
            .and_then(|_| write_u16(&mut self.file, FILE_VERSION))
            .and_then(|_| self.file.write_all(&[0, 0]))
            .and_then(|_| write_u64(&mut self.file, created_unix_seconds))
            .and_then(|_| write_u32(&mut self.file, block.sample_rate_hz))
            .and_then(|_| write_u16(&mut self.file, block.channels.len() as u16))
            .and_then(|_| write_u16(&mut self.file, block.source_id.len() as u16))
            .and_then(|_| write_u16(&mut self.file, self.metadata.len() as u16))
            .and_then(|_| self.file.write_all(block.source_id.as_bytes()))
            .map_err(|error| StorageError::io("write recording header", &self.path, error))?;
        for channel in &block.channels {
            self.file
                .write_all(&[channel.stream, channel.channel])
                .map_err(|error| StorageError::io("write channel map", &self.path, error))?;
        }
        for (key, value) in &self.metadata {
            write_u16(&mut self.file, key.len() as u16)
                .and_then(|_| write_u16(&mut self.file, value.len() as u16))
                .and_then(|_| self.file.write_all(key.as_bytes()))
                .and_then(|_| self.file.write_all(value.as_bytes()))
                .map_err(|error| StorageError::io("write recording metadata", &self.path, error))?;
        }
        self.info = Some(info);
        Ok(())
    }

    fn write_block(&mut self, block: &SampleBlock) -> Result<(), StorageError> {
        if self.finished {
            return Err(StorageError::new("recording has already been finalized"));
        }
        block
            .validate()
            .map_err(|error| StorageError::new(error.to_string()))?;
        if self.info.is_none() {
            self.write_header(block)?;
        }
        let info = self.info.as_ref().expect("recording header must exist");
        if block.format_version != SAMPLE_BLOCK_FORMAT_VERSION
            || block.sample_rate_hz != info.sample_rate_hz
            || block.source_id != info.source_id
            || block.channels != info.channels
        {
            return Err(StorageError::new(
                "SampleBlock format changed during one recording",
            ));
        }
        if let Some(expected) = self.next_sequence {
            if block.sequence != expected {
                return Err(StorageError::new(format!(
                    "block sequence mismatch: expected {expected}, got {}",
                    block.sequence
                )));
            }
        }
        if let Some(expected) = self.next_sample_index {
            if block.first_sample_index != expected
                && !block.flags.contains(SampleFlags::DISCONTINUITY)
            {
                return Err(StorageError::new(format!(
                    "sample index mismatch without discontinuity flag: expected {expected}, got {}",
                    block.first_sample_index
                )));
            }
        }

        let payload = encode_block(block)?;
        let payload_len = u32::try_from(payload.len())
            .map_err(|_| StorageError::new("recording block exceeds u32 length"))?;
        self.file
            .write_all(BLOCK_MARKER)
            .and_then(|_| write_u32(&mut self.file, payload_len))
            .and_then(|_| write_u32(&mut self.file, crc32(&payload)))
            .and_then(|_| self.file.write_all(&payload))
            .map_err(|error| StorageError::io("write recording block", &self.path, error))?;

        self.block_count += 1;
        self.samples_per_channel += block.samples_per_channel as u64;
        self.next_sequence = Some(block.sequence.saturating_add(1));
        self.next_sample_index = Some(block.last_sample_index().saturating_add(1));
        Ok(())
    }

    pub fn finalize(&mut self) -> Result<(), StorageError> {
        if self.finished {
            return Ok(());
        }
        if self.info.is_none() {
            return Err(StorageError::new(
                "cannot finalize a recording with no data blocks",
            ));
        }

        self.file
            .write_all(FOOTER_MARKER)
            .and_then(|_| write_u64(&mut self.file, self.block_count))
            .and_then(|_| write_u64(&mut self.file, self.samples_per_channel))
            .and_then(|_| self.file.flush())
            .and_then(|_| self.file.sync_data())
            .map_err(|error| StorageError::io("finalize recording footer", &self.path, error))?;
        self.file
            .seek(SeekFrom::Start(COMPLETE_OFFSET))
            .and_then(|_| self.file.write_all(&[1]))
            .and_then(|_| self.file.flush())
            .and_then(|_| self.file.sync_all())
            .map_err(|error| StorageError::io("mark recording complete", &self.path, error))?;
        self.finished = true;
        if let Some(info) = &mut self.info {
            info.complete = true;
        }
        Ok(())
    }
}

impl Sink for RawRecorderSink {
    fn name(&self) -> &str {
        "raw-recorder"
    }

    fn consume(&mut self, block: &SampleBlock) -> Result<(), PipelineError> {
        self.write_block(block)
            .map_err(|error| PipelineError::new("raw-recorder", error.to_string()))
    }

    fn finish(&mut self) -> Result<(), PipelineError> {
        self.finalize()
            .map_err(|error| PipelineError::new("raw-recorder", error.to_string()))
    }
}

pub struct FileReplaySource {
    path: PathBuf,
    file: File,
    info: RecordingInfo,
    ended: bool,
    blocks_read: u64,
    samples_per_channel: u64,
    next_sequence: Option<u64>,
    next_sample_index: Option<u64>,
}

impl FileReplaySource {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let path = path.into();
        let mut file =
            File::open(&path).map_err(|error| StorageError::io("open recording", &path, error))?;
        let info = read_header(&mut file, &path)?;
        Ok(Self {
            path,
            file,
            info,
            ended: false,
            blocks_read: 0,
            samples_per_channel: 0,
            next_sequence: None,
            next_sample_index: None,
        })
    }

    pub fn info(&self) -> &RecordingInfo {
        &self.info
    }

    fn read_next(&mut self) -> Result<Option<SampleBlock>, StorageError> {
        if self.ended {
            return Ok(None);
        }

        let mut marker = [0_u8; 4];
        let first_byte_count = self
            .file
            .read(&mut marker[..1])
            .map_err(|error| StorageError::io("read record marker", &self.path, error))?;
        if first_byte_count == 0 {
            self.ended = true;
            if self.info.complete {
                return Err(StorageError::new(
                    "complete recording ended without a valid footer",
                ));
            }
            return Ok(None);
        }
        self.file
            .read_exact(&mut marker[1..])
            .map_err(|error| StorageError::io("read complete record marker", &self.path, error))?;

        if &marker == FOOTER_MARKER {
            let recorded_blocks = read_u64(&mut self.file)
                .map_err(|error| StorageError::io("read footer block count", &self.path, error))?;
            let recorded_samples = read_u64(&mut self.file)
                .map_err(|error| StorageError::io("read footer sample count", &self.path, error))?;
            if recorded_blocks != self.blocks_read || recorded_samples != self.samples_per_channel {
                return Err(StorageError::new(format!(
                    "footer count mismatch: blocks {recorded_blocks}/{}, samples {recorded_samples}/{}",
                    self.blocks_read, self.samples_per_channel
                )));
            }
            self.ended = true;
            return Ok(None);
        }
        if &marker != BLOCK_MARKER {
            return Err(StorageError::new(format!(
                "invalid recording marker: {marker:02x?}"
            )));
        }

        let payload_len = read_u32(&mut self.file)
            .map_err(|error| StorageError::io("read block length", &self.path, error))?
            as usize;
        let expected_crc = read_u32(&mut self.file)
            .map_err(|error| StorageError::io("read block checksum", &self.path, error))?;
        if payload_len == 0 || payload_len > MAX_RECORD_BYTES {
            return Err(StorageError::new(format!(
                "invalid recording block length: {payload_len}"
            )));
        }
        let mut payload = vec![0_u8; payload_len];
        self.file
            .read_exact(&mut payload)
            .map_err(|error| StorageError::io("read block payload", &self.path, error))?;
        let actual_crc = crc32(&payload);
        if actual_crc != expected_crc {
            return Err(StorageError::new(format!(
                "recording block checksum mismatch: expected {expected_crc:#010x}, got {actual_crc:#010x}"
            )));
        }

        let block = decode_block(&payload, &self.info)?;
        if let Some(expected) = self.next_sequence {
            if block.sequence != expected {
                return Err(StorageError::new(format!(
                    "replay sequence mismatch: expected {expected}, got {}",
                    block.sequence
                )));
            }
        }
        if let Some(expected) = self.next_sample_index {
            if block.first_sample_index != expected
                && !block.flags.contains(SampleFlags::DISCONTINUITY)
            {
                return Err(StorageError::new(format!(
                    "replay sample index mismatch: expected {expected}, got {}",
                    block.first_sample_index
                )));
            }
        }

        self.blocks_read += 1;
        self.samples_per_channel += block.samples_per_channel as u64;
        self.next_sequence = Some(block.sequence.saturating_add(1));
        self.next_sample_index = Some(block.last_sample_index().saturating_add(1));
        Ok(Some(block))
    }
}

impl Source for FileReplaySource {
    fn next_block(&mut self) -> Result<Option<SampleBlock>, SourceError> {
        self.read_next()
            .map_err(|error| SourceError::new(error.to_string()))
    }
}

fn read_header(file: &mut File, path: &Path) -> Result<RecordingInfo, StorageError> {
    let mut magic = [0_u8; 8];
    file.read_exact(&mut magic)
        .map_err(|error| StorageError::io("read recording magic", path, error))?;
    if &magic != FILE_MAGIC {
        return Err(StorageError::new("not an RHSRAW recording"));
    }
    let version =
        read_u16(file).map_err(|error| StorageError::io("read recording version", path, error))?;
    if version != FILE_VERSION {
        return Err(StorageError::new(format!(
            "unsupported recording version: {version}"
        )));
    }
    let mut flags = [0_u8; 2];
    file.read_exact(&mut flags)
        .map_err(|error| StorageError::io("read recording flags", path, error))?;
    let complete = flags[0] == 1;
    if flags[0] > 1 {
        return Err(StorageError::new("invalid recording complete flag"));
    }
    let created_unix_seconds =
        read_u64(file).map_err(|error| StorageError::io("read creation time", path, error))?;
    let sample_rate_hz =
        read_u32(file).map_err(|error| StorageError::io("read sample rate", path, error))?;
    let channel_count = read_u16(file)
        .map_err(|error| StorageError::io("read channel count", path, error))?
        as usize;
    let source_len = read_u16(file)
        .map_err(|error| StorageError::io("read source id length", path, error))?
        as usize;
    let metadata_count = read_u16(file)
        .map_err(|error| StorageError::io("read metadata count", path, error))?
        as usize;
    if sample_rate_hz == 0 || channel_count == 0 || source_len == 0 {
        return Err(StorageError::new("invalid recording header dimensions"));
    }
    let mut source_bytes = vec![0_u8; source_len];
    file.read_exact(&mut source_bytes)
        .map_err(|error| StorageError::io("read source id", path, error))?;
    let source_id = String::from_utf8(source_bytes)
        .map_err(|_| StorageError::new("recording source_id is not valid UTF-8"))?;
    let mut channels = Vec::with_capacity(channel_count);
    for _ in 0..channel_count {
        let mut channel = [0_u8; 2];
        file.read_exact(&mut channel)
            .map_err(|error| StorageError::io("read channel map", path, error))?;
        channels.push(ChannelId {
            stream: channel[0],
            channel: channel[1],
        });
    }
    let mut metadata = Vec::with_capacity(metadata_count);
    for _ in 0..metadata_count {
        let key_len = read_u16(file)
            .map_err(|error| StorageError::io("read metadata key length", path, error))?
            as usize;
        let value_len = read_u16(file)
            .map_err(|error| StorageError::io("read metadata value length", path, error))?
            as usize;
        if key_len == 0 {
            return Err(StorageError::new(
                "recording metadata key must not be empty",
            ));
        }
        let mut key = vec![0_u8; key_len];
        let mut value = vec![0_u8; value_len];
        file.read_exact(&mut key)
            .and_then(|_| file.read_exact(&mut value))
            .map_err(|error| StorageError::io("read recording metadata", path, error))?;
        metadata.push((
            String::from_utf8(key)
                .map_err(|_| StorageError::new("metadata key is not valid UTF-8"))?,
            String::from_utf8(value)
                .map_err(|_| StorageError::new("metadata value is not valid UTF-8"))?,
        ));
    }
    Ok(RecordingInfo {
        version,
        complete,
        created_unix_seconds,
        sample_rate_hz,
        source_id,
        channels,
        metadata,
    })
}

fn validate_metadata(metadata: &[(String, String)]) -> Result<(), StorageError> {
    if metadata.len() > u16::MAX as usize {
        return Err(StorageError::new("too many metadata entries"));
    }
    for (key, value) in metadata {
        if key.is_empty() {
            return Err(StorageError::new("metadata key must not be empty"));
        }
        if key.len() > u16::MAX as usize || value.len() > u16::MAX as usize {
            return Err(StorageError::new("metadata key or value is too long"));
        }
    }
    Ok(())
}

pub fn sha256_file(path: impl AsRef<Path>) -> Result<String, StorageError> {
    let path = path.as_ref();
    let bytes =
        fs::read(path).map_err(|error| StorageError::io("read file for SHA-256", path, error))?;
    Ok(hex_sha256(&bytes))
}

fn hex_sha256(input: &[u8]) -> String {
    const INITIAL: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut padded = input.to_vec();
    let bit_length = (input.len() as u64).wrapping_mul(8);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_length.to_be_bytes());

    let mut state = INITIAL;
    for chunk in padded.chunks_exact(64) {
        let mut words = [0_u32; 64];
        for (index, bytes) in chunk.chunks_exact(4).take(16).enumerate() {
            words[index] = u32::from_be_bytes(bytes.try_into().expect("SHA-256 word is 4 bytes"));
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let sum1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(sum1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let sum0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = sum0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    let mut output = String::with_capacity(64);
    for word in state {
        use fmt::Write as _;
        write!(&mut output, "{word:08x}").expect("writing to String cannot fail");
    }
    output
}

fn encode_block(block: &SampleBlock) -> Result<Vec<u8>, StorageError> {
    let sample_count = u32::try_from(block.samples.len())
        .map_err(|_| StorageError::new("too many amplifier samples in one block"))?;
    let ttl_count = u32::try_from(block.ttl_in.len())
        .map_err(|_| StorageError::new("too many TTL samples in one block"))?;
    let fixed_bytes = 8 + 8 + 4 + 4 + 4 + 4;
    let data_bytes = block
        .samples
        .len()
        .checked_add(block.ttl_in.len())
        .and_then(|words| words.checked_mul(2))
        .ok_or_else(|| StorageError::new("recording payload size overflow"))?;
    let mut payload = Vec::with_capacity(fixed_bytes + data_bytes);
    append_u64(&mut payload, block.sequence);
    append_u64(&mut payload, block.first_sample_index);
    append_u32(&mut payload, block.samples_per_channel);
    append_u32(&mut payload, block.flags.bits());
    append_u32(&mut payload, sample_count);
    append_u32(&mut payload, ttl_count);
    for sample in &block.samples {
        append_u16(&mut payload, *sample);
    }
    for ttl in &block.ttl_in {
        append_u16(&mut payload, *ttl);
    }
    Ok(payload)
}

fn decode_block(payload: &[u8], info: &RecordingInfo) -> Result<SampleBlock, StorageError> {
    let mut cursor = Cursor::new(payload);
    let sequence = read_u64(&mut cursor).map_err(StorageError::from_io)?;
    let first_sample_index = read_u64(&mut cursor).map_err(StorageError::from_io)?;
    let samples_per_channel = read_u32(&mut cursor).map_err(StorageError::from_io)?;
    let flags = SampleFlags::from_bits(read_u32(&mut cursor).map_err(StorageError::from_io)?);
    let sample_count = read_u32(&mut cursor).map_err(StorageError::from_io)? as usize;
    let ttl_count = read_u32(&mut cursor).map_err(StorageError::from_io)? as usize;

    let expected_samples = info
        .channels
        .len()
        .checked_mul(samples_per_channel as usize)
        .ok_or_else(|| StorageError::new("decoded sample dimensions overflow"))?;
    if sample_count != expected_samples {
        return Err(StorageError::new(format!(
            "decoded sample count mismatch: expected {expected_samples}, got {sample_count}"
        )));
    }
    if ttl_count != 0 && ttl_count != samples_per_channel as usize {
        return Err(StorageError::new(format!(
            "decoded TTL count mismatch: expected 0 or {samples_per_channel}, got {ttl_count}"
        )));
    }
    let required_payload_bytes = cursor.position() as usize
        + sample_count
            .checked_add(ttl_count)
            .and_then(|words| words.checked_mul(2))
            .ok_or_else(|| StorageError::new("decoded payload length overflow"))?;
    if required_payload_bytes != payload.len() {
        return Err(StorageError::new(format!(
            "decoded payload length mismatch: expected {required_payload_bytes}, got {}",
            payload.len()
        )));
    }

    let mut samples = Vec::with_capacity(sample_count);
    for _ in 0..sample_count {
        samples.push(read_u16(&mut cursor).map_err(StorageError::from_io)?);
    }
    let mut ttl_in = Vec::with_capacity(ttl_count);
    for _ in 0..ttl_count {
        ttl_in.push(read_u16(&mut cursor).map_err(StorageError::from_io)?);
    }
    SampleBlock::new(
        info.source_id.clone(),
        sequence,
        first_sample_index,
        info.sample_rate_hz,
        info.channels.clone(),
        samples_per_channel,
        samples,
        ttl_in,
        flags,
    )
    .map_err(|error| StorageError::new(error.to_string()))
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn append_u16(buffer: &mut Vec<u8>, value: u16) {
    buffer.extend_from_slice(&value.to_le_bytes());
}

fn append_u32(buffer: &mut Vec<u8>, value: u32) {
    buffer.extend_from_slice(&value.to_le_bytes());
}

fn append_u64(buffer: &mut Vec<u8>, value: u64) {
    buffer.extend_from_slice(&value.to_le_bytes());
}

fn write_u16(writer: &mut impl Write, value: u16) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn write_u32(writer: &mut impl Write, value: u32) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn write_u64(writer: &mut impl Write, value: u64) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn read_u16(reader: &mut impl Read) -> io::Result<u16> {
    let mut bytes = [0_u8; 2];
    reader.read_exact(&mut bytes)?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32(reader: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0_u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(reader: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0_u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

#[derive(Debug)]
pub struct StorageError {
    message: String,
}

impl StorageError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn io(operation: &str, path: &Path, error: io::Error) -> Self {
        Self::new(format!(
            "{operation} failed for {}: {error}",
            path.display()
        ))
    }

    fn from_io(error: io::Error) -> Self {
        Self::new(format!("decode recording block failed: {error}"))
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl StdError for StorageError {}

#[cfg(test)]
mod tests {
    use super::*;
    use rhs_pipeline::{
        CancellationToken, PassThrough, PipelineRunner, Processor, SyntheticConfig, SyntheticSource,
    };
    use std::fs;

    fn temp_path(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "rhs-storage-{label}-{}-{unique}.rhsraw",
            std::process::id()
        ))
    }

    fn synthetic_blocks(count: u64) -> Vec<SampleBlock> {
        let mut source = SyntheticSource::new(SyntheticConfig {
            channel_count: 3,
            samples_per_block: 7,
            total_blocks: Some(count),
            ..SyntheticConfig::default()
        })
        .unwrap();
        let mut blocks = Vec::new();
        while let Some(block) = source.next_block().unwrap() {
            blocks.push(block);
        }
        blocks
    }

    #[test]
    fn round_trips_complete_recording_exactly() {
        let path = temp_path("round-trip");
        let expected = synthetic_blocks(5);
        let mut recorder = RawRecorderSink::create(&path).unwrap();
        for block in &expected {
            recorder.consume(block).unwrap();
        }
        recorder.finish().unwrap();

        let mut replay = FileReplaySource::open(&path).unwrap();
        assert!(replay.info().complete);
        let mut actual = Vec::new();
        while let Some(block) = replay.next_block().unwrap() {
            actual.push(block);
        }
        assert_eq!(actual, expected);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn round_trips_recording_metadata() {
        let path = temp_path("metadata");
        let expected = synthetic_blocks(1);
        let metadata = vec![
            ("device_serial".to_owned(), "TEST-RHS2116".to_owned()),
            ("bitfile_sha256".to_owned(), "0123456789abcdef".to_owned()),
        ];
        let mut recorder = RawRecorderSink::create_with_metadata(&path, metadata.clone()).unwrap();
        recorder.consume(&expected[0]).unwrap();
        recorder.finish().unwrap();

        let replay = FileReplaySource::open(&path).unwrap();
        assert_eq!(replay.info().metadata, metadata);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            hex_sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn unfinalized_recording_is_marked_incomplete_but_recoverable() {
        let path = temp_path("incomplete");
        let expected = synthetic_blocks(1);
        {
            let mut recorder = RawRecorderSink::create(&path).unwrap();
            recorder.consume(&expected[0]).unwrap();
        }

        let mut replay = FileReplaySource::open(&path).unwrap();
        assert!(!replay.info().complete);
        assert_eq!(replay.next_block().unwrap(), Some(expected[0].clone()));
        assert!(replay.next_block().unwrap().is_none());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn detects_corrupted_block_payload() {
        let path = temp_path("corrupt");
        let expected = synthetic_blocks(1);
        let mut recorder = RawRecorderSink::create(&path).unwrap();
        recorder.consume(&expected[0]).unwrap();
        recorder.finish().unwrap();
        drop(recorder);

        let mut bytes = fs::read(&path).unwrap();
        let marker = bytes
            .windows(BLOCK_MARKER.len())
            .position(|window| window == BLOCK_MARKER)
            .unwrap();
        bytes[marker + 12] ^= 0x01;
        fs::write(&path, bytes).unwrap();

        let mut replay = FileReplaySource::open(&path).unwrap();
        let error = replay.next_block().unwrap_err();
        assert!(error.to_string().contains("checksum mismatch"));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn rejects_sequence_gap_without_discontinuity_flag() {
        let path = temp_path("gap");
        let mut blocks = synthetic_blocks(2);
        blocks[1].sequence = 9;
        let mut recorder = RawRecorderSink::create(&path).unwrap();
        recorder.consume(&blocks[0]).unwrap();
        let error = recorder.consume(&blocks[1]).unwrap_err();
        assert!(error.to_string().contains("sequence mismatch"));
        drop(recorder);

        let mut replay = FileReplaySource::open(&path).unwrap();
        assert!(!replay.info().complete);
        assert_eq!(replay.next_block().unwrap(), Some(blocks[0].clone()));
        assert!(replay.next_block().unwrap().is_none());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn pipeline_runner_records_a_replayable_file() {
        let path = temp_path("pipeline");
        let mut source = SyntheticSource::new(SyntheticConfig {
            channel_count: 4,
            samples_per_block: 11,
            total_blocks: Some(8),
            ..SyntheticConfig::default()
        })
        .unwrap();
        let mut processors: Vec<Box<dyn Processor>> = vec![Box::new(PassThrough)];
        let mut sinks: Vec<Box<dyn Sink>> = vec![Box::new(RawRecorderSink::create(&path).unwrap())];
        let runner = PipelineRunner::new(CancellationToken::new());

        let report = runner
            .run(&mut source, &mut processors, &mut sinks)
            .unwrap();
        assert_eq!(report.source_blocks, 8);
        assert_eq!(report.emitted_blocks, 8);
        drop(sinks);

        let mut replay = FileReplaySource::open(&path).unwrap();
        let mut replayed = 0;
        while replay.next_block().unwrap().is_some() {
            replayed += 1;
        }
        assert_eq!(replayed, 8);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn detects_truncated_complete_recording() {
        let path = temp_path("truncated");
        let expected = synthetic_blocks(1);
        let mut recorder = RawRecorderSink::create(&path).unwrap();
        recorder.consume(&expected[0]).unwrap();
        recorder.finish().unwrap();
        drop(recorder);

        let file = OpenOptions::new().write(true).open(&path).unwrap();
        let truncated_len = file.metadata().unwrap().len() - 5;
        file.set_len(truncated_len).unwrap();
        drop(file);

        let mut replay = FileReplaySource::open(&path).unwrap();
        assert!(replay.next_block().unwrap().is_some());
        let error = replay.next_block().unwrap_err();
        assert!(error.to_string().contains("footer"));
        fs::remove_file(path).unwrap();
    }
}
