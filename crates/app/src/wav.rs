//! Writing signals to WAV files: 32-bit float samples (`WAVE_FORMAT_IEEE_FLOAT`), so
//! nothing is rounded or clipped and values past ±1 survive. Plain data, no editor:
//! the one-pass run records what the output nodes deliver and writes it here.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// The WAV format code of IEEE float samples.
const IEEE_FLOAT: u16 = 3;
/// Bytes per sample.
const SAMPLE_BYTES: u16 = 4;

/// The bytes of a WAV file of `samples`, interleaved with `channels` per frame, at
/// `sample_rate` frames per second. Fails if they don't fit a WAV file (4 GiB).
pub(crate) fn wav_bytes(samples: &[f32], channels: u16, sample_rate: u32) -> io::Result<Vec<u8>> {
    let too_big = || io::Error::new(io::ErrorKind::InvalidInput, "too long for a WAV file");
    let data_len =
        u32::try_from(samples.len() * usize::from(SAMPLE_BYTES)).map_err(|_| too_big())?;
    // RIFF header, "fmt " chunk of 18 bytes (with the empty extension size, as float
    // formats should have), "fact" chunk, "data" chunk.
    let riff_len = data_len.checked_add(4 + 26 + 12 + 8).ok_or_else(too_big)?;
    let block_align = channels * SAMPLE_BYTES;
    let frames = samples.len() as u32 / u32::from(channels.max(1));

    let mut bytes = Vec::with_capacity(riff_len as usize + 8);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&riff_len.to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&18_u32.to_le_bytes());
    bytes.extend_from_slice(&IEEE_FLOAT.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * u32::from(block_align)).to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&(SAMPLE_BYTES * 8).to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(b"fact");
    bytes.extend_from_slice(&4_u32.to_le_bytes());
    bytes.extend_from_slice(&frames.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    Ok(bytes)
}

/// Writes [`wav_bytes`] to `path`, through a temporary file next to it, so a failed
/// write leaves an older file as it was.
pub(crate) fn write_wav(
    path: &Path,
    samples: &[f32],
    channels: u16,
    sample_rate: u32,
) -> io::Result<()> {
    let bytes = wav_bytes(samples, channels, sample_rate)?;
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = std::path::PathBuf::from(temp);
    let written = fs::File::create(&temp).and_then(|mut file| {
        file.write_all(&bytes)?;
        file.sync_all()
    });
    written
        .and_then(|()| fs::rename(&temp, path))
        .inspect_err(|_| {
            let _ = fs::remove_file(&temp);
        })
}

/// The samples of the WAV file `bytes` that [`wav_bytes`] made.
#[cfg(test)]
pub(crate) fn samples_of(bytes: &[u8]) -> Vec<f32> {
    const DATA: usize = 58;
    let (samples, _) = bytes[DATA..].as_chunks::<4>();
    samples
        .iter()
        .map(|&sample| f32::from_le_bytes(sample))
        .collect()
}

/// The channels of the WAV file `bytes` that [`wav_bytes`] made.
#[cfg(test)]
pub(crate) fn channels_of(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[22], bytes[23]])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16_at(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([bytes[at], bytes[at + 1]])
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn a_float_wav_has_its_format_and_the_samples_as_they_are() {
        // Two stereo frames, one past full scale.
        let samples = [0.5, -0.5, 1.5, -2.0];

        let bytes = wav_bytes(&samples, 2, 48_000).unwrap();

        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(u32_at(&bytes, 4) as usize, bytes.len() - 8);
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(u16_at(&bytes, 20), IEEE_FLOAT);
        assert_eq!(u16_at(&bytes, 22), 2, "channels");
        assert_eq!(u32_at(&bytes, 24), 48_000, "sample rate");
        assert_eq!(u32_at(&bytes, 28), 48_000 * 8, "bytes per second");
        assert_eq!(u16_at(&bytes, 32), 8, "block align");
        assert_eq!(u16_at(&bytes, 34), 32, "bits per sample");
        assert_eq!(&bytes[38..42], b"fact");
        assert_eq!(u32_at(&bytes, 46), 2, "frames");
        assert_eq!(&bytes[50..54], b"data");
        assert_eq!(u32_at(&bytes, 54), 16);
        assert_eq!(samples_of(&bytes), samples);
    }

    #[test]
    fn writes_the_file_and_leaves_no_temporary_one() {
        let dir = std::env::temp_dir().join(format!("v-signal-nodes-wav-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.wav");

        write_wav(&path, &[0.25; 10], 1, 1000).unwrap();

        assert_eq!(
            fs::read(&path).unwrap(),
            wav_bytes(&[0.25; 10], 1, 1000).unwrap()
        );
        assert!(!dir.join("out.wav.tmp").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
