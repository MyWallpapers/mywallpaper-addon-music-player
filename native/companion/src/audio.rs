//! Optional Windows system-audio spectrum capture for the music-player addon.
//!
//! This module only starts a WASAPI loopback client when the caller requests
//! visualization. It captures the default shared-mode render endpoint, reduces
//! the real PCM stream to 32 logarithmic FFT bands, and shuts its worker down
//! when [`SpectrumMonitor`] is stopped or dropped.

use std::error::Error;
use std::fmt;
#[cfg(windows)]
use std::sync::mpsc;
use std::sync::mpsc::{Receiver, SyncSender};
#[cfg(windows)]
use std::thread;
use std::thread::JoinHandle;

/// Number of normalized frequency bands produced for each visualizer frame.
pub const SPECTRUM_BAND_COUNT: usize = 32;

/// A spectrum frame computed from samples captured from the Windows audio mix.
/// Each value is finite and normalized to the inclusive range `0.0..=1.0`.
#[derive(Clone, Debug, PartialEq)]
pub struct SpectrumFrame {
    pub bands: [f32; SPECTRUM_BAND_COUNT],
}

/// A capture or setup error from the optional spectrum monitor.
#[derive(Debug)]
pub struct AudioError {
    message: String,
}

impl AudioError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for AudioError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for AudioError {}

/// Owns the optional loopback worker. `stop` and `Drop` both signal and join it.
pub struct SpectrumMonitor {
    stop_tx: SyncSender<()>,
    worker: Option<JoinHandle<()>>,
}

impl SpectrumMonitor {
    /// Stop capture and wait for the worker to release its WASAPI resources.
    pub fn stop(mut self) {
        self.stop_worker();
    }

    fn stop_worker(&mut self) {
        let _ = self.stop_tx.try_send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for SpectrumMonitor {
    fn drop(&mut self) {
        self.stop_worker();
    }
}

/// Start optional WASAPI loopback capture and return its owner and frame stream.
///
/// Capture is scoped to the default render endpoint's shared audio mix. This
/// does not record or retain audio: samples are reduced in memory to the 32-band
/// spectrum and discarded. On non-Windows platforms this returns an explicit
/// unsupported-platform error.
pub fn start_spectrum_monitor() -> Result<(SpectrumMonitor, Receiver<SpectrumFrame>), AudioError> {
    #[cfg(windows)]
    {
        let (stop_tx, stop_rx) = mpsc::sync_channel(1);
        let (frames_tx, frames_rx) = mpsc::sync_channel(1);
        let (startup_tx, startup_rx) = mpsc::sync_channel(1);

        let worker = thread::Builder::new()
            .name("music-player-wasapi-spectrum".to_owned())
            .spawn(move || windows_capture::run_worker(stop_rx, frames_tx, startup_tx))
            .map_err(|error| {
                AudioError::new(format!("Could not start spectrum worker: {error}"))
            })?;

        match startup_rx.recv() {
            Ok(Ok(())) => Ok((
                SpectrumMonitor {
                    stop_tx,
                    worker: Some(worker),
                },
                frames_rx,
            )),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                let _ = worker.join();
                Err(AudioError::new(
                    "Spectrum worker exited before WASAPI capture was ready.",
                ))
            }
        }
    }

    #[cfg(not(windows))]
    {
        Err(AudioError::new(
            "WASAPI loopback spectrum capture is available only on Windows.",
        ))
    }
}

#[cfg(windows)]
mod windows_capture {
    use super::{AudioError, SPECTRUM_BAND_COUNT, SpectrumFrame};
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::ptr;
    use std::slice;
    use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError};
    use std::time::{Duration, Instant};
    use windows::Win32::Media::Audio::{
        AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
        IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX,
        eMultimedia, eRender,
    };
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
        CoUninitialize,
    };

    const FFT_SIZE: usize = 4096;
    const OUTPUT_INTERVAL: Duration = Duration::from_millis(67);
    const CAPTURE_POLL_INTERVAL: Duration = Duration::from_millis(5);
    const WAVE_FORMAT_PCM: u16 = 0x0001;
    const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
    const WAVE_FORMAT_EXTENSIBLE: u16 = 0xfffe;
    const MIN_FREQUENCY_HZ: f32 = 35.0;
    const MAX_FREQUENCY_HZ: f32 = 20_000.0;
    const FLOOR_DB: f32 = -60.0;
    const CEILING_DB: f32 = -6.0;

    pub(super) fn run_worker(
        stop_rx: Receiver<()>,
        frames_tx: SyncSender<SpectrumFrame>,
        startup_tx: SyncSender<Result<(), AudioError>>,
    ) {
        let _apartment = match ComApartment::initialize() {
            Ok(apartment) => apartment,
            Err(error) => {
                let _ = startup_tx.send(Err(error));
                return;
            }
        };

        let mut session = match CaptureSession::open() {
            Ok(session) => session,
            Err(error) => {
                let _ = startup_tx.send(Err(error));
                return;
            }
        };

        if startup_tx.send(Ok(())).is_err() {
            session.stop();
            return;
        }

        session.run(stop_rx, frames_tx);
        session.stop();
    }

    struct ComApartment;

    impl ComApartment {
        fn initialize() -> Result<Self, AudioError> {
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() }
                .map_err(|error| win_error("Could not initialize COM for WASAPI", error))?;
            Ok(Self)
        }
    }

    impl Drop for ComApartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    struct MixFormatAllocation(*mut WAVEFORMATEX);

    impl Drop for MixFormatAllocation {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CoTaskMemFree(Some(self.0 as *const c_void)) };
            }
        }
    }

    struct CaptureSession {
        audio_client: IAudioClient,
        capture_client: IAudioCaptureClient,
        format: SampleFormat,
        analyzer: SpectrumAnalyzer,
    }

    impl CaptureSession {
        fn open() -> Result<Self, AudioError> {
            let enumerator: IMMDeviceEnumerator = unsafe {
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|error| win_error("Could not enumerate audio devices", error))?
            };
            // Music playback uses the multimedia role, which Windows assigns
            // to the user's default endpoint for music and movies.
            let endpoint = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia) }
                .map_err(|error| win_error("Could not open the default render endpoint", error))?;
            let audio_client: IAudioClient = unsafe { endpoint.Activate(CLSCTX_ALL, None) }
                .map_err(|error| win_error("Could not activate the render endpoint", error))?;
            let mix_format_ptr = unsafe { audio_client.GetMixFormat() }
                .map_err(|error| win_error("Could not read the render mix format", error))?;
            let mix_format_allocation = MixFormatAllocation(mix_format_ptr);
            if mix_format_ptr.is_null() {
                return Err(AudioError::new(
                    "Windows returned an empty format for the render endpoint.",
                ));
            }

            let format = unsafe { SampleFormat::from_wave_format(mix_format_ptr)? };
            unsafe {
                audio_client.Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK,
                    0,
                    0,
                    mix_format_ptr,
                    None,
                )
            }
            .map_err(|error| win_error("Could not initialize WASAPI loopback capture", error))?;
            let capture_client: IAudioCaptureClient = unsafe { audio_client.GetService() }
                .map_err(|error| win_error("Could not acquire the WASAPI capture buffer", error))?;
            unsafe { audio_client.Start() }
                .map_err(|error| win_error("Could not start WASAPI loopback capture", error))?;

            drop(mix_format_allocation);
            Ok(Self {
                audio_client,
                capture_client,
                analyzer: SpectrumAnalyzer::new(format.sample_rate),
                format,
            })
        }

        fn run(&mut self, stop_rx: Receiver<()>, frames_tx: SyncSender<SpectrumFrame>) {
            let mut next_output = Instant::now();
            loop {
                match stop_rx.try_recv() {
                    Ok(()) | Err(TryRecvError::Disconnected) => break,
                    Err(TryRecvError::Empty) => {}
                }

                if self.drain_capture_packets().is_err() {
                    // A disconnected or invalidated endpoint closes the frame
                    // stream. The caller observes receiver closure and can stop
                    // the visualization cleanly.
                    break;
                }

                let now = Instant::now();
                if now >= next_output {
                    if let Some(bands) = self.analyzer.spectrum() {
                        let frame = SpectrumFrame { bands };
                        match frames_tx.try_send(frame) {
                            Ok(()) | Err(TrySendError::Full(_)) => {}
                            Err(TrySendError::Disconnected(_)) => break,
                        }
                    }
                    next_output = now + OUTPUT_INTERVAL;
                }

                let remaining = next_output.saturating_duration_since(Instant::now());
                let wait = remaining.min(CAPTURE_POLL_INTERVAL);
                match stop_rx.recv_timeout(wait) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
        }

        fn drain_capture_packets(&mut self) -> Result<(), AudioError> {
            loop {
                let packet_frames =
                    unsafe { self.capture_client.GetNextPacketSize() }.map_err(|error| {
                        win_error("Could not inspect the WASAPI capture buffer", error)
                    })?;
                if packet_frames == 0 {
                    return Ok(());
                }

                self.read_capture_packet()?;
            }
        }

        fn read_capture_packet(&mut self) -> Result<(), AudioError> {
            let mut data: *mut u8 = ptr::null_mut();
            let mut frame_count = 0u32;
            let mut flags = 0u32;
            unsafe {
                self.capture_client
                    .GetBuffer(&mut data, &mut frame_count, &mut flags, None, None)
            }
            .map_err(|error| win_error("Could not read a WASAPI capture packet", error))?;

            if frame_count == 0 {
                return Ok(());
            }

            let copy_result = (|| {
                if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 {
                    for _ in 0..frame_count {
                        self.analyzer.push_sample(0.0);
                    }
                    Ok(())
                } else if data.is_null() {
                    Err(AudioError::new(
                        "Windows returned a null pointer for a non-silent audio packet.",
                    ))
                } else {
                    let byte_count = (frame_count as usize)
                        .checked_mul(self.format.block_align)
                        .ok_or_else(|| AudioError::new("WASAPI packet size overflowed."))?;
                    if byte_count > isize::MAX as usize {
                        return Err(AudioError::new(
                            "WASAPI packet size exceeds the addressable buffer limit.",
                        ));
                    }
                    let packet = unsafe { slice::from_raw_parts(data, byte_count) };
                    self.format
                        .copy_mono_samples(packet, frame_count, &mut self.analyzer)
                }
            })();

            let release_result = unsafe { self.capture_client.ReleaseBuffer(frame_count) }
                .map_err(|error| win_error("Could not release a WASAPI capture packet", error));

            copy_result?;
            release_result
        }

        fn stop(&mut self) {
            let _ = unsafe { self.audio_client.Stop() };
        }
    }

    #[derive(Clone, Copy)]
    enum Encoding {
        Float32,
        Float64,
        Pcm { valid_bits: u16 },
    }

    #[derive(Clone, Copy)]
    struct SampleFormat {
        sample_rate: u32,
        channels: usize,
        block_align: usize,
        bytes_per_sample: usize,
        container_bits: u16,
        encoding: Encoding,
    }

    impl SampleFormat {
        unsafe fn from_wave_format(format_ptr: *const WAVEFORMATEX) -> Result<Self, AudioError> {
            if format_ptr.is_null() {
                return Err(AudioError::new("Windows returned a null mix format."));
            }
            let format = unsafe { &*format_ptr };
            let channels = format.nChannels as usize;
            let sample_rate = format.nSamplesPerSec;
            let container_bits = format.wBitsPerSample;
            if channels == 0 || sample_rate == 0 || container_bits == 0 || container_bits % 8 != 0 {
                return Err(AudioError::new(
                    "The Windows mix format has invalid dimensions.",
                ));
            }

            let (encoding_tag, valid_bits) = match format.wFormatTag {
                WAVE_FORMAT_PCM => (WAVE_FORMAT_PCM, container_bits),
                WAVE_FORMAT_IEEE_FLOAT => (WAVE_FORMAT_IEEE_FLOAT, container_bits),
                WAVE_FORMAT_EXTENSIBLE => {
                    if format.cbSize < 22 {
                        return Err(AudioError::new(
                            "The Windows extensible mix format is missing its subtype.",
                        ));
                    }
                    let format_bytes = format_ptr.cast::<u8>();
                    let valid_bits_ptr =
                        unsafe { format_bytes.add(size_of::<WAVEFORMATEX>()).cast::<u16>() };
                    let valid_bits = unsafe { ptr::read_unaligned(valid_bits_ptr) };
                    let subformat_ptr = unsafe { format_bytes.add(size_of::<WAVEFORMATEX>() + 6) };
                    let subformat = unsafe { slice::from_raw_parts(subformat_ptr, 16) };
                    let encoding_tag = if subformat == pcm_subformat_guid() {
                        WAVE_FORMAT_PCM
                    } else if subformat == ieee_float_subformat_guid() {
                        WAVE_FORMAT_IEEE_FLOAT
                    } else {
                        return Err(AudioError::new(
                            "The Windows mix format uses an unsupported audio subtype.",
                        ));
                    };
                    (
                        encoding_tag,
                        if valid_bits == 0 {
                            container_bits
                        } else {
                            valid_bits
                        },
                    )
                }
                _ => {
                    return Err(AudioError::new(
                        "The Windows mix format uses an unsupported sample encoding.",
                    ));
                }
            };

            let encoding = match encoding_tag {
                WAVE_FORMAT_IEEE_FLOAT if container_bits == 32 && valid_bits == 32 => {
                    Encoding::Float32
                }
                WAVE_FORMAT_IEEE_FLOAT if container_bits == 64 && valid_bits == 64 => {
                    Encoding::Float64
                }
                WAVE_FORMAT_PCM
                    if matches!(container_bits, 8 | 16 | 24 | 32)
                        && valid_bits > 0
                        && valid_bits <= container_bits =>
                {
                    Encoding::Pcm { valid_bits }
                }
                _ => {
                    return Err(AudioError::new(
                        "The Windows mix format uses an unsupported bit depth.",
                    ));
                }
            };

            let bytes_per_sample = (container_bits / 8) as usize;
            let block_align = format.nBlockAlign as usize;
            if block_align == 0 || block_align != channels.saturating_mul(bytes_per_sample) {
                return Err(AudioError::new(
                    "The Windows mix format has an unsupported frame alignment.",
                ));
            }

            Ok(Self {
                sample_rate,
                channels,
                block_align,
                bytes_per_sample,
                container_bits,
                encoding,
            })
        }

        fn copy_mono_samples(
            &self,
            packet: &[u8],
            frame_count: u32,
            analyzer: &mut SpectrumAnalyzer,
        ) -> Result<(), AudioError> {
            let expected_size = (frame_count as usize)
                .checked_mul(self.block_align)
                .ok_or_else(|| AudioError::new("WASAPI packet size overflowed."))?;
            if packet.len() < expected_size {
                return Err(AudioError::new("WASAPI returned a truncated audio packet."));
            }

            for frame in packet[..expected_size].chunks_exact(self.block_align) {
                let mut mono = 0.0f32;
                for channel in 0..self.channels {
                    let start = channel * self.bytes_per_sample;
                    mono += self.decode_sample(&frame[start..start + self.bytes_per_sample]);
                }
                analyzer.push_sample(mono / self.channels as f32);
            }
            Ok(())
        }

        fn decode_sample(&self, sample: &[u8]) -> f32 {
            match self.encoding {
                Encoding::Float32 => {
                    let value = f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]);
                    if value.is_finite() {
                        value.clamp(-1.0, 1.0)
                    } else {
                        0.0
                    }
                }
                Encoding::Float64 => {
                    let value = f64::from_le_bytes([
                        sample[0], sample[1], sample[2], sample[3], sample[4], sample[5],
                        sample[6], sample[7],
                    ]) as f32;
                    if value.is_finite() {
                        value.clamp(-1.0, 1.0)
                    } else {
                        0.0
                    }
                }
                Encoding::Pcm { valid_bits } => {
                    if self.container_bits == 8 {
                        let shift = self.container_bits - valid_bits;
                        let unsigned = (sample[0] >> shift) as i32;
                        let midpoint = 1i32 << (valid_bits - 1);
                        (unsigned - midpoint) as f32 / midpoint as f32
                    } else {
                        let raw = match self.container_bits {
                            16 => i16::from_le_bytes([sample[0], sample[1]]) as i32,
                            24 => {
                                let packed = (sample[0] as i32)
                                    | ((sample[1] as i32) << 8)
                                    | ((sample[2] as i32) << 16);
                                (packed << 8) >> 8
                            }
                            32 => i32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]),
                            _ => return 0.0,
                        };
                        let aligned = raw >> (self.container_bits - valid_bits);
                        let scale = (1i64 << (valid_bits - 1)) as f32;
                        (aligned as f32 / scale).clamp(-1.0, 1.0)
                    }
                }
            }
        }
    }

    fn pcm_subformat_guid() -> &'static [u8; 16] {
        &[
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38,
            0x9b, 0x71,
        ]
    }

    fn ieee_float_subformat_guid() -> &'static [u8; 16] {
        &[
            0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38,
            0x9b, 0x71,
        ]
    }

    struct SpectrumAnalyzer {
        sample_rate: f32,
        ring: Box<[f32; FFT_SIZE]>,
        cursor: usize,
        filled: usize,
        work: Box<[Complex; FFT_SIZE]>,
        smoothed: [f32; SPECTRUM_BAND_COUNT],
    }

    impl SpectrumAnalyzer {
        fn new(sample_rate: u32) -> Self {
            Self {
                sample_rate: sample_rate as f32,
                ring: Box::new([0.0; FFT_SIZE]),
                cursor: 0,
                filled: 0,
                work: Box::new([Complex::default(); FFT_SIZE]),
                smoothed: [0.0; SPECTRUM_BAND_COUNT],
            }
        }

        fn push_sample(&mut self, sample: f32) {
            self.ring[self.cursor] = if sample.is_finite() {
                sample.clamp(-1.0, 1.0)
            } else {
                0.0
            };
            self.cursor = (self.cursor + 1) % FFT_SIZE;
            self.filled = (self.filled + 1).min(FFT_SIZE);
        }

        fn spectrum(&mut self) -> Option<[f32; SPECTRUM_BAND_COUNT]> {
            if self.filled < FFT_SIZE {
                return None;
            }

            for index in 0..FFT_SIZE {
                let sample = self.ring[(self.cursor + index) % FFT_SIZE];
                let window = 0.5
                    - 0.5 * (std::f32::consts::TAU * index as f32 / (FFT_SIZE - 1) as f32).cos();
                self.work[index] = Complex {
                    real: sample * window,
                    imaginary: 0.0,
                };
            }
            fft(&mut self.work[..]);

            let max_hz = MAX_FREQUENCY_HZ.min(self.sample_rate * 0.46);
            let min_hz = MIN_FREQUENCY_HZ.min(max_hz * 0.5);
            if max_hz <= min_hz {
                return Some([0.0; SPECTRUM_BAND_COUNT]);
            }
            let ratio = max_hz / min_hz;
            let mut bands = [0.0; SPECTRUM_BAND_COUNT];
            for (band_index, band) in bands.iter_mut().enumerate() {
                let low_hz = min_hz * ratio.powf(band_index as f32 / SPECTRUM_BAND_COUNT as f32);
                let high_hz =
                    min_hz * ratio.powf((band_index + 1) as f32 / SPECTRUM_BAND_COUNT as f32);
                let first_bin = ((low_hz * FFT_SIZE as f32 / self.sample_rate).floor() as usize)
                    .max(1)
                    .min(FFT_SIZE / 2);
                let end_bin = ((high_hz * FFT_SIZE as f32 / self.sample_rate).ceil() as usize)
                    .max(first_bin + 1)
                    .min(FFT_SIZE / 2 + 1);

                let mut power = 0.0f32;
                let mut count = 0usize;
                for bin in first_bin..end_bin {
                    let coefficient = self.work[bin];
                    let magnitude =
                        coefficient.real.hypot(coefficient.imaginary) * (2.0 / FFT_SIZE as f32);
                    power += magnitude * magnitude;
                    count += 1;
                }
                let amplitude = if count == 0 {
                    0.0
                } else {
                    (power / count as f32).sqrt()
                };
                let decibels = 20.0 * amplitude.max(1.0e-6).log10();
                let raw = ((decibels - FLOOR_DB) / (CEILING_DB - FLOOR_DB)).clamp(0.0, 1.0);
                let response = if raw > self.smoothed[band_index] {
                    0.65
                } else {
                    0.28
                };
                self.smoothed[band_index] += (raw - self.smoothed[band_index]) * response;
                *band = self.smoothed[band_index];
            }
            Some(bands)
        }
    }

    #[derive(Clone, Copy, Default)]
    struct Complex {
        real: f32,
        imaginary: f32,
    }

    fn fft(values: &mut [Complex]) {
        let length = values.len();
        let mut reverse = 0usize;
        for index in 1..length {
            let mut bit = length >> 1;
            while reverse & bit != 0 {
                reverse ^= bit;
                bit >>= 1;
            }
            reverse ^= bit;
            if index < reverse {
                values.swap(index, reverse);
            }
        }

        let mut block_size = 2usize;
        while block_size <= length {
            let angle = -std::f32::consts::TAU / block_size as f32;
            let step = Complex {
                real: angle.cos(),
                imaginary: angle.sin(),
            };
            for block_start in (0..length).step_by(block_size) {
                let mut twiddle = Complex {
                    real: 1.0,
                    imaginary: 0.0,
                };
                let half = block_size / 2;
                for offset in 0..half {
                    let even = values[block_start + offset];
                    let odd_source = values[block_start + offset + half];
                    let odd = Complex {
                        real: odd_source.real * twiddle.real
                            - odd_source.imaginary * twiddle.imaginary,
                        imaginary: odd_source.real * twiddle.imaginary
                            + odd_source.imaginary * twiddle.real,
                    };
                    values[block_start + offset] = Complex {
                        real: even.real + odd.real,
                        imaginary: even.imaginary + odd.imaginary,
                    };
                    values[block_start + offset + half] = Complex {
                        real: even.real - odd.real,
                        imaginary: even.imaginary - odd.imaginary,
                    };
                    twiddle = Complex {
                        real: twiddle.real * step.real - twiddle.imaginary * step.imaginary,
                        imaginary: twiddle.real * step.imaginary + twiddle.imaginary * step.real,
                    };
                }
            }
            block_size <<= 1;
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn pcm_mix_format_decodes_signed_samples_to_normalized_audio() {
            let wave_format = WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_PCM,
                nChannels: 2,
                nSamplesPerSec: 48_000,
                nAvgBytesPerSec: 192_000,
                nBlockAlign: 4,
                wBitsPerSample: 16,
                cbSize: 0,
            };
            let decoded = unsafe {
                SampleFormat::from_wave_format(&wave_format)
                    .expect("ordinary stereo PCM format should be supported")
            };

            assert_eq!(decoded.channels, 2);
            assert_eq!(decoded.decode_sample(&[0x00, 0x80]), -1.0);
            assert_eq!(decoded.decode_sample(&[0x00, 0x00]), 0.0);
            assert!((decoded.decode_sample(&[0xff, 0x7f]) - 0.999_969_5).abs() < 0.000_001);
        }

        #[test]
        fn fft_places_a_real_tone_in_its_log_frequency_band() {
            let mut analyzer = SpectrumAnalyzer::new(48_000);
            for index in 0..FFT_SIZE {
                let phase = std::f32::consts::TAU * 440.0 * index as f32 / 48_000.0;
                analyzer.push_sample(0.5 * phase.sin());
            }

            let bands = analyzer
                .spectrum()
                .expect("a complete FFT window should produce spectrum bands");
            let peak_band = bands
                .iter()
                .enumerate()
                .max_by(|left, right| left.1.total_cmp(right.1))
                .map(|(index, _)| index)
                .expect("the spectrum has bands");

            assert!(
                (11..=13).contains(&peak_band),
                "unexpected peak band {peak_band}"
            );
            assert!(bands[peak_band] > 0.4, "tone should produce visible energy");
        }
    }

    fn win_error(context: &str, error: windows::core::Error) -> AudioError {
        AudioError::new(format!("{context}: {error}"))
    }
}
