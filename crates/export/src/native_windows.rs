//! Windows H.264 export with bounded, deferred requests served by the job's render caller.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use effectcraft_project::{
    Comp,
    render_queue::{Channels, FieldRender, OutputFormat},
};
use effectcraft_time::{FrameRate, Tick};
use windows::{
    Foundation::{PropertyValue, TimeSpan, TypedEventHandler},
    Media::{
        Core::{
            AudioStreamDescriptor, MediaStreamSample, MediaStreamSource, MediaStreamSourceErrorStatus, MediaStreamSourceSampleRequest,
            MediaStreamSourceSampleRequestDeferral, MediaStreamSourceSampleRequestedEventArgs, MediaStreamSourceStartingEventArgs, VideoStreamDescriptor,
        },
        MediaProperties::{AudioEncodingProperties, MediaEncodingProfile, MediaEncodingSubtypes, VideoEncodingProperties, VideoEncodingQuality},
        Transcoding::MediaTranscoder,
    },
    Storage::{
        FileAccessMode, StorageFile,
        Streams::{DataWriter, IRandomAccessStream},
    },
    Win32::Media::MediaFoundation::{
        MF_MT_DEFAULT_STRIDE, MF_MT_INTERLACE_MODE, MF_MT_MPEG2_LEVEL, MF_MT_TRANSFER_FUNCTION, MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_VIDEO_PRIMARIES,
        MF_MT_YUV_MATRIX, MFNominalRange_0_255, MFNominalRange_16_235, MFVideoInterlace_Progressive, MFVideoPrimaries_BT709, MFVideoTransFunc_709,
        MFVideoTransferMatrix_BT709,
    },
    core::{Error as WinError, HRESULT, HSTRING, Interface},
};
use windows_future::AsyncStatus;

use crate::{Cx, ExportError, Report, Result, State, batch_size, wants_audio};

impl From<WinError> for ExportError {
    fn from(error: WinError) -> Self {
        Self::Encode(format!("Windows H.264 export: {error}"))
    }
}

const REQUEST_CAPACITY: usize = 4;
const BATCH_BYTES: u64 = 512 * 1024 * 1024;

fn invalid(message: &str) -> ExportError {
    ExportError::Encode(format!("Windows H.264 export: {message}"))
}
fn win_failure() -> WinError {
    WinError::from_hresult(HRESULT(0x80004005_u32 as i32))
}

fn encoder_mode(mode: Option<&std::ffi::OsStr>) -> (bool, bool) {
    match mode {
        None => (true, true),
        Some(mode) if mode == "software" || mode == "off" => (false, false),
        Some(mode) if mode == "hardware" => (true, false),
        Some(mode) if mode == "auto" => (true, true),
        Some(mode) => {
            log::warn!("invalid EFFECTCRAFT_H264_ENCODER={mode:?}; retaining software export");
            (false, false)
        }
    }
}

/// Windows file exports try the measured native encoder, retaining pre-render software fallback.
pub(crate) async fn try_movie(job: &Cx<'_>, comp: &Comp, w: u32, h: u32, st: &mut State<'_>) -> Result<Option<Report>> {
    if job.output.format != OutputFormat::H264 {
        return Ok(None);
    }
    let (enabled, fallback) = encoder_mode(std::env::var_os("EFFECTCRAFT_H264_ENCODER").as_deref());
    if !enabled {
        return Ok(None);
    }
    if let Err(error) = supported(job, comp, w, h) {
        if fallback && matches!(error, ExportError::Unsupported(_)) {
            log::warn!("{error}; retaining software H.264 export");
            return Ok(None);
        }
        return Err(error);
    }
    match movie(job, comp, w, h, st).await {
        Err(error @ ExportError::Unsupported(_)) if fallback && st.done == 0 => {
            log::warn!("{error}; retaining software H.264 export");
            Ok(None)
        }
        result => result.map(Some),
    }
}

fn supported(job: &Cx<'_>, comp: &Comp, w: u32, h: u32) -> Result<()> {
    if job.sink.is_some() {
        return Err(ExportError::Unsupported("Windows hardware export requires a file destination; select software for an in-memory sink".into()));
    }
    if job.settings.field_render != FieldRender::Off {
        return Err(ExportError::Unsupported("Windows hardware export currently requires progressive frames; select software for field rendering".into()));
    }
    let rate = job.settings.rate(comp);
    if rate.num <= 0 || rate.den <= 0 || u32::try_from(rate.num).is_err() || u32::try_from(rate.den).is_err() {
        return Err(invalid("frame rate must be a positive 32-bit rational"));
    }
    let blocks = u64::from(w).div_ceil(16) * u64::from(h).div_ceil(16);
    if w < 64
        || h < 64
        || !w.is_multiple_of(2)
        || !h.is_multiple_of(2)
        || blocks > 36_864
        || i128::from(blocks) * i128::from(rate.num) > 2_073_600 * i128::from(rate.den)
    {
        return Err(ExportError::Unsupported(
            "Windows hardware H.264 export requires even dimensions of at least 64 pixels and a size/rate within level 5.2; select software for this profile"
                .into(),
        ));
    }
    if wants_audio(job) && !matches!(job.output.audio_sample_rate, 44_100 | 48_000) {
        return Err(ExportError::Unsupported("Windows AAC export currently supports 44.1 or 48 kHz; select software for other sample rates".into()));
    }
    Ok(())
}

fn frame_time(frame: u64, rate: FrameRate) -> Result<TimeSpan> {
    if rate.num <= 0 || rate.den <= 0 {
        return Err(invalid("non-positive frame rate"));
    }
    let numerator =
        i128::from(frame).checked_mul(i128::from(rate.den)).and_then(|x| x.checked_mul(10_000_000)).ok_or_else(|| invalid("frame timestamp overflow"))?;
    Ok(TimeSpan { Duration: i64::try_from(numerator / i128::from(rate.num)).map_err(|_| invalid("frame timestamp exceeds the Windows time range"))? })
}

fn sample_time(sample: u64, rate: u32) -> Result<TimeSpan> {
    if rate == 0 {
        return Err(invalid("zero audio sample rate"));
    }
    Ok(TimeSpan { Duration: i64::try_from(i128::from(sample) * 10_000_000 / i128::from(rate)).map_err(|_| invalid("audio timestamp overflow"))? })
}

fn sample(bytes: &[u8], start: TimeSpan, end: TimeSpan) -> Result<MediaStreamSample> {
    let duration = end.Duration.checked_sub(start.Duration).filter(|&d| d > 0).ok_or_else(|| invalid("sample has no positive duration"))?;
    let writer = DataWriter::new()?;
    writer.WriteBytes(bytes)?;
    let sample = MediaStreamSample::CreateFromBuffer(&writer.DetachBuffer()?, start)?;
    sample.SetDuration(TimeSpan { Duration: duration })?;
    Ok(sample)
}

struct Pending {
    request: MediaStreamSourceSampleRequest,
    deferral: Option<MediaStreamSourceSampleRequestDeferral>,
    video: bool,
}
impl Pending {
    fn complete(&mut self) -> windows::core::Result<()> {
        match self.deferral.take() {
            Some(d) => d.Complete(),
            None => Ok(()),
        }
    }
}
impl Drop for Pending {
    fn drop(&mut self) {
        if let Err(error) = self.complete() {
            log::warn!("cannot complete Windows export sample request: {error}");
        }
    }
}

struct Session {
    source: MediaStreamSource,
    stream: IRandomAccessStream,
    starting: Option<i64>,
    requested: Option<i64>,
    closed: bool,
}
impl Session {
    fn stop_requests(&mut self) -> Result<()> {
        if let Some(token) = self.requested.take() {
            self.source.RemoveSampleRequested(token)?;
        }
        if let Some(token) = self.starting.take() {
            self.source.RemoveStarting(token)?;
        }
        Ok(())
    }
    fn close(&mut self) -> Result<()> {
        self.stream.Close()?;
        self.closed = true;
        Ok(())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if let Err(error) = self.stop_requests() {
            log::warn!("cannot remove Windows export callbacks: {error}");
        }
        if !self.closed
            && let Err(error) = self.close()
        {
            log::warn!("cannot close Windows export stream: {error}");
        }
    }
}

fn video_properties(w: u32, h: u32, rate: FrameRate, encoded: bool, bitrate: u32) -> Result<VideoEncodingProperties> {
    let properties =
        if encoded { VideoEncodingProperties::CreateH264()? } else { VideoEncodingProperties::CreateUncompressed(&MediaEncodingSubtypes::Bgra8()?, w, h)? };
    properties.SetWidth(w)?;
    properties.SetHeight(h)?;
    properties.FrameRate()?.SetNumerator(u32::try_from(rate.num).map_err(|_| invalid("frame rate numerator overflow"))?)?;
    properties.FrameRate()?.SetDenominator(u32::try_from(rate.den).map_err(|_| invalid("frame rate denominator overflow"))?)?;
    properties.PixelAspectRatio()?.SetNumerator(1)?;
    properties.PixelAspectRatio()?.SetDenominator(1)?;
    let attrs = properties.Properties()?;
    attrs.Insert(MF_MT_INTERLACE_MODE, &PropertyValue::CreateUInt32(MFVideoInterlace_Progressive.0 as u32)?)?;
    if encoded {
        properties.SetProfileId(100)?;
        properties.SetBitrate(bitrate)?;
        attrs.Insert(MF_MT_MPEG2_LEVEL, &PropertyValue::CreateUInt32(52)?)?;
    } else {
        let stride = w.checked_mul(4).ok_or_else(|| invalid("video stride overflow"))?;
        attrs.Insert(MF_MT_DEFAULT_STRIDE, &PropertyValue::CreateUInt32(stride)?)?;
    }
    for (key, value) in [
        (MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0),
        (MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0),
        (MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0),
        (MF_MT_VIDEO_NOMINAL_RANGE, if encoded { MFNominalRange_16_235.0 } else { MFNominalRange_0_255.0 }),
    ] {
        attrs.Insert(key, &PropertyValue::CreateUInt32(value as u32)?)?;
    }
    Ok(properties)
}

fn callback_failure(failure: &Mutex<Option<String>>, sender: Option<&MediaStreamSource>, error: &WinError) {
    let notification = sender.map(|s| s.NotifyError(MediaStreamSourceErrorStatus::Other));
    failure
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_or_insert_with(|| format!("Windows export sample callback: {error}; notification={notification:?}"));
}

async fn movie(job: &Cx<'_>, comp: &Comp, w: u32, h: u32, st: &mut State<'_>) -> Result<Report> {
    let rate = job.settings.rate(comp);
    let with_audio = wants_audio(job);
    let sr = job.output.audio_sample_rate;
    let channels = if job.output.audio_channels == 1 { 1 } else { 2 };
    let bitrate = job.output.bitrate_kbps.max(100).checked_mul(1000).ok_or_else(|| invalid("bitrate exceeds the Windows encoder range"))?;
    let profile = MediaEncodingProfile::CreateMp4(VideoEncodingQuality::HD1080p)?;
    profile.SetVideo(&video_properties(w, h, rate, true, bitrate)?)?;
    let video = VideoStreamDescriptor::Create(&video_properties(w, h, rate, false, bitrate)?)?;
    let source = if with_audio {
        profile.SetAudio(&AudioEncodingProperties::CreateAac(sr, channels, 160_000 * channels)?)?;
        let audio = AudioEncodingProperties::CreatePcm(sr, channels, 32)?;
        audio.SetSubtype(&MediaEncodingSubtypes::Float()?)?;
        MediaStreamSource::CreateFromDescriptors(&video, &AudioStreamDescriptor::Create(&audio)?)?
    } else {
        profile.SetAudio(None::<&AudioEncodingProperties>)?;
        MediaStreamSource::CreateFromDescriptor(&video)?
    };
    source.SetCanSeek(false)?;
    source.SetBufferTime(TimeSpan { Duration: 0 })?;
    source.SetDuration(frame_time(st.total, rate)?)?;
    let path = job.place(job.path, 1);
    let mut output = job.create(&path)?;
    let native_path = output.external_path()?;
    let native_path = native_path.to_str().ok_or_else(|| invalid("output path cannot be represented as Unicode"))?;
    let native_path = if let Some(unc) = native_path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        native_path.strip_prefix(r"\\?\").unwrap_or(native_path).replace('/', r"\")
    };
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(native_path))?.join()?;
    let stream = file.OpenAsync(FileAccessMode::ReadWrite)?.join()?;
    let mut session = Session { source, stream, starting: None, requested: None, closed: false };
    let failure = Arc::new(Mutex::new(None::<String>));
    let start_failure = failure.clone();
    session.starting =
        Some(session.source.Starting(&TypedEventHandler::<MediaStreamSource, MediaStreamSourceStartingEventArgs>::new(move |sender, args| {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let args = args.as_ref().ok_or_else(win_failure)?;
                args.Request()?.SetActualStartPosition(TimeSpan { Duration: 0 })
            }))
            .unwrap_or_else(|_| Err(win_failure()));
            if let Err(error) = &result {
                callback_failure(&start_failure, sender.as_ref(), error);
            }
            result
        }))?);
    let (send, requests) = mpsc::sync_channel::<Pending>(REQUEST_CAPACITY);
    let sample_failure = failure.clone();
    session.requested = Some(session.source.SampleRequested(&TypedEventHandler::<MediaStreamSource, MediaStreamSourceSampleRequestedEventArgs>::new(
        move |sender, args| {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let args = args.as_ref().ok_or_else(win_failure)?;
                let request = args.Request()?;
                let descriptor = request.StreamDescriptor()?;
                let video = descriptor.cast::<VideoStreamDescriptor>().is_ok();
                if !video && descriptor.cast::<AudioStreamDescriptor>().is_err() {
                    return Err(win_failure());
                }
                let deferral = request.GetDeferral()?;
                send.try_send(Pending { request, deferral: Some(deferral), video }).map_err(|error| {
                    drop(error);
                    win_failure()
                })
            }))
            .unwrap_or_else(|_| Err(win_failure()));
            if let Err(error) = &result {
                callback_failure(&sample_failure, sender.as_ref(), error);
            }
            result
        },
    ))?);
    let transcoder = MediaTranscoder::new()?;
    transcoder.SetHardwareAccelerationEnabled(true)?;
    transcoder.SetAlwaysReencode(true)?;
    let prepare = transcoder.PrepareMediaStreamSourceTranscodeAsync(&session.source, &session.stream, &profile)?;
    let started = Instant::now();
    while prepare.Status()? == AsyncStatus::Started {
        if let Err(error) = st.advance(0) {
            if let Err(cancel) = prepare.Cancel() {
                log::warn!("cancel Windows export preparation: {cancel}");
            }
            return Err(error);
        }
        if started.elapsed() > Duration::from_secs(60) {
            prepare.Cancel()?;
            return Err(invalid("encoder preparation timed out; select software or check the video driver"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let preparation =
        prepare.GetResults().map_err(|e| ExportError::Unsupported(format!("Windows cannot prepare this H.264 profile: {e}; select software export")))?;
    if !preparation.CanTranscode()? {
        return Err(ExportError::Unsupported(format!("Windows rejected this H.264 profile ({:?}); select software export", preparation.FailureReason()?)));
    }
    let operation = preparation.TranscodeAsync().map_err(|e| invalid(&format!("start transcoding: {e}")))?;
    let result = deliver(job, comp, w, h, st, &session.source, &requests, &failure, &operation).await;
    if result.is_err() {
        if let Err(error) = session.source.NotifyError(MediaStreamSourceErrorStatus::Other) {
            log::warn!("notify failed Windows export: {error}");
        }
        if let Err(error) = operation.Cancel() {
            log::warn!("cancel failed Windows export: {error}");
        }
    }
    session.stop_requests()?;
    while let Ok(pending) = requests.try_recv() {
        drop(pending);
    }
    // Closing the native stream precedes dropping or publishing our owned provisional file.
    if result.is_ok() {
        session.stream.FlushAsync()?.join()?;
    }
    session.close()?;
    result?;
    let bytes = output.finish_external()?;
    Ok(Report { path, frames: 0, width: w, height: h, seconds: 0.0, bytes, audio: with_audio, log: None, overflow: vec![] })
}

#[allow(clippy::too_many_arguments)]
async fn deliver(
    job: &Cx<'_>,
    comp: &Comp,
    w: u32,
    h: u32,
    st: &mut State<'_>,
    source: &MediaStreamSource,
    requests: &mpsc::Receiver<Pending>,
    failure: &Mutex<Option<String>>,
    operation: &windows_future::IAsyncActionWithProgress<f64>,
) -> Result<()> {
    let rate = job.settings.rate(comp);
    let sr = job.output.audio_sample_rate;
    let chans = if job.output.audio_channels == 1 { 1_usize } else { 2 };
    let (span_start, span_end) = job.settings.span(comp);
    let first_audio = span_start.to_units_floor(i64::from(sr));
    let audio_total = u64::try_from(span_end.to_units_floor(i64::from(sr)).checked_sub(first_audio).ok_or_else(|| invalid("audio span overflow"))?)
        .map_err(|_| invalid("negative audio span"))?;
    let pixel_bytes = u64::from(w).checked_mul(u64::from(h)).and_then(|n| n.checked_mul(4)).ok_or_else(|| invalid("pixel buffer size overflow"))?;
    let batch = batch_size().min((BATCH_BYTES / pixel_bytes.saturating_mul(5).max(1)).max(1));
    let expected_pixels = usize::try_from(pixel_bytes).map_err(|_| invalid("pixel buffer exceeds addressable memory"))?;
    let mut frames = VecDeque::new();
    let mut next_video = 0_u64;
    let mut next_audio = 0_u64;
    let mut last_request = Instant::now();
    while operation.Status()? == AsyncStatus::Started {
        st.advance(0)?;
        if let Some(error) = failure.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
            return Err(invalid(&error));
        }
        let mut pending = match requests.recv_timeout(Duration::from_millis(10)) {
            Ok(request) => request,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if last_request.elapsed() > Duration::from_secs(60) {
                    return Err(invalid("encoder stopped requesting samples; select software or check the video driver"));
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(invalid("encoder sample callbacks disconnected")),
        };
        if pending.video {
            if next_video >= st.total {
                pending.request.SetSample(None::<&MediaStreamSample>)?;
            } else {
                if frames.is_empty() {
                    pending.request.ReportSampleProgress(0)?;
                    let end = next_video.saturating_add(batch).min(st.total);
                    let channels = if job.output.channels == Channels::Alpha { Channels::Alpha } else { Channels::Rgb };
                    frames = job.frames(comp, (next_video..end).collect(), |_, img| job.pixels(&img, comp, channels, w, h)).await?.into();
                }
                let mut bytes = frames.pop_front().ok_or_else(|| invalid("renderer did not return the requested frame"))?;
                if bytes.len() != expected_pixels {
                    return Err(invalid("renderer returned a malformed video buffer"));
                }
                for pixel in bytes.as_chunks_mut::<4>().0 {
                    pixel.swap(0, 2);
                }
                let end = next_video.checked_add(1).ok_or_else(|| invalid("frame counter overflow"))?;
                pending.request.SetSample(&sample(&bytes, frame_time(next_video, rate)?, frame_time(end, rate)?)?)?;
                next_video = end;
                st.advance(1)?;
            }
        } else if next_audio >= audio_total {
            pending.request.SetSample(None::<&MediaStreamSample>)?;
        } else {
            let count = usize::try_from((audio_total - next_audio).min(1024)).map_err(|_| invalid("audio batch overflow"))?;
            let absolute = first_audio
                .checked_add(i64::try_from(next_audio).map_err(|_| invalid("audio cursor overflow"))?)
                .ok_or_else(|| invalid("audio cursor overflow"))?;
            let start = Tick::from_units(absolute, i64::from(sr));
            let start = if start.to_units_floor(i64::from(sr)) < absolute { Tick(start.0.saturating_add(1)) } else { start };
            let samples = crate::encode::mix(job, start, count, sr);
            if samples.len() != count.saturating_mul(chans) || samples.iter().any(|s| !s.is_finite()) {
                return Err(invalid("audio mixer returned malformed or non-finite samples"));
            }
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(samples.len().checked_mul(4).ok_or_else(|| invalid("audio buffer overflow"))?)
                .map_err(|e| invalid(&format!("audio allocation: {e}")))?;
            for value in samples {
                bytes.extend_from_slice(&value.clamp(-1.0, 1.0).to_le_bytes());
            }
            let end = next_audio.checked_add(count as u64).ok_or_else(|| invalid("audio cursor overflow"))?;
            pending.request.SetSample(&sample(&bytes, sample_time(next_audio, sr)?, sample_time(end, sr)?)?)?;
            next_audio = end;
        }
        pending.complete()?;
        last_request = Instant::now();
    }
    operation.GetResults().map_err(|e| invalid(&format!("transcode completion after {next_video} frames and {next_audio} audio samples: {e}")))?;
    if let Some(error) = failure.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
        return Err(invalid(&error));
    }
    if next_video != st.total || (wants_audio(job) && next_audio != audio_total) {
        source.NotifyError(MediaStreamSourceErrorStatus::Other)?;
        return Err(invalid("encoder completed before every requested frame and audio sample was delivered"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Job, JobOptions};
    use effectcraft_color::Label;
    use effectcraft_project::render_queue::{AudioOutput, Crop, OutputModule, RenderSettings, Resize, ResizeQuality};
    use effectcraft_project::{Footage, FootageKind, ItemId, ItemKind, LayerSource, Project, Solid, build};
    use effectcraft_raster::Image;
    use effectcraft_render::FootageSource;
    use std::path::Path;
    use std::process::Command;

    #[test]
    fn windows_h264_defaults_to_native_with_pre_render_fallback_and_software_overrides() {
        use std::ffi::OsStr;
        assert_eq!(encoder_mode(None), (true, true));
        assert_eq!(encoder_mode(Some(OsStr::new("auto"))), (true, true));
        assert_eq!(encoder_mode(Some(OsStr::new("hardware"))), (true, false));
        for mode in ["software", "off", "invalid"] {
            assert_eq!(encoder_mode(Some(OsStr::new(mode))), (false, false));
        }
    }

    #[test]
    fn rational_video_and_audio_timestamps_do_not_accumulate_rounding() {
        let mut duration = 0;
        for frame in 0..120 {
            let a = frame_time(frame, FrameRate::FPS_120).unwrap().Duration;
            let b = frame_time(frame + 1, FrameRate::FPS_120).unwrap().Duration;
            assert!((83_333..=83_334).contains(&(b - a)));
            duration += b - a;
        }
        assert_eq!(duration, 10_000_000);
        assert_eq!(frame_time(120_000, FrameRate::FPS_119_88).unwrap().Duration, 10_010_000_000);
        assert_eq!(sample_time(44_100, 44_100).unwrap().Duration, 10_000_000);
        assert_eq!(sample_time(48_000 * 3600, 48_000).unwrap().Duration, 36_000_000_000);
    }

    #[test]
    fn hostile_timestamps_return_errors() {
        assert!(frame_time(1, FrameRate { num: 0, den: 1 }).is_err());
        assert!(frame_time(1, FrameRate { num: 1, den: -1 }).is_err());
        assert!(frame_time(u64::MAX, FrameRate { num: 1, den: i64::MAX }).is_err());
        assert!(sample_time(1, 0).is_err());
        assert!(sample_time(u64::MAX, 1).is_err());
    }

    struct Tone;
    impl FootageSource for Tone {
        fn frame(&self, _: ItemId, _: &Footage, _: Tick) -> Option<Arc<Image>> {
            None
        }
        fn audio(&self, _: ItemId, _: &Footage, start: Tick, frames: usize, rate: u32) -> Option<Vec<f32>> {
            let first = start.to_units_floor(i64::from(rate));
            Some(
                (0..frames)
                    .flat_map(|i| [440.0_f32, 880.0].map(|hz| (std::f32::consts::TAU * hz * (first + i as i64) as f32 / rate as f32).sin() * 0.25))
                    .collect(),
            )
        }
    }

    fn fixture() -> (Project, ItemId) {
        let mut p = Project::default();
        p.settings.gpu_acceleration = false;
        let mut comp = Comp::new(640, 360, FrameRate::FPS_120, Tick::from_seconds_f64(1.0));
        comp.background = [0.1, 0.1, 0.4];
        let red = p.add_item("Red", Label::Red, None, ItemKind::Solid(Solid { color: [1.0, 0.0, 0.0], width: 64, height: 48, pixel_aspect: 1.0 }));
        let mut layer = build::layer(&mut p, &comp, "Red", LayerSource::Solid { item: red }, (64, 48), None);
        layer.in_point = comp.frame_rate.tick_of(60);
        comp.layers.push(layer);
        let tone = p.add_item(
            "Generated tone",
            Label::SeaFoam,
            None,
            ItemKind::Footage(Footage { kind: FootageKind::Audio, duration: comp.duration, has_audio: true, ..Default::default() }),
        );
        comp.layers.push(build::layer(&mut p, &comp, "Tone", LayerSource::Footage { item: tone }, (0, 0), None));
        let cid = p.add_item("Comp", Label::Sandstone, None, ItemKind::Comp(comp.into()));
        (p, cid)
    }

    fn export_fixture(path: &Path, cancel: bool) -> Result<Report> {
        let (project, cid) = fixture();
        let settings = RenderSettings::default();
        let output = OutputModule {
            audio: AudioOutput::On,
            audio_sample_rate: 48_000,
            audio_channels: 2,
            crop: Crop { enabled: true, top: 32, left: 64, bottom: 32, right: 64, ..Default::default() },
            resize: Resize { enabled: true, width: 640, height: 360, lock_aspect: false, quality: ResizeQuality::High },
            ..OutputModule::for_format(OutputFormat::H264)
        };
        let job = Job {
            project: &project,
            comp: cid,
            footage: &Tone,
            expr: None,
            accel: None,
            settings: &settings,
            output: &output,
            path: path.to_str().unwrap(),
            sink: None,
            nested_switches: true,
            options: JobOptions::default(),
        };
        let cx = Cx::new(&job);
        let mut progress = |p: &crate::Progress| !cancel || p.done < 12;
        let mut state = State { t0: web_time::Instant::now(), total: 120, done: 0, progress: &mut progress };
        let comp = cx.comp().unwrap();
        supported(&cx, comp, 640, 360)?;
        let result = effectcraft_render::passes::block_on(try_movie(&cx, comp, 640, 360, &mut state))?
            .ok_or_else(|| invalid("native acceptance fixture selected software; run with default, auto or hardware encoding"));
        if result.is_ok() {
            assert_eq!(state.done, 120);
        }
        result
    }

    #[test]
    #[ignore = "requires Windows media codecs and an external FFmpeg/ffprobe oracle; run explicitly for hardware-export acceptance"]
    fn hardware_export_preserves_timing_audio_and_destination() {
        let directory = std::env::temp_dir().join(format!(
            "effectcraft-native-export-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("movie.mp4");
        let report = export_fixture(&path, false).unwrap();
        assert!(report.audio && report.bytes > 0);
        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-count_frames",
                "-show_entries",
                "stream=codec_name,width,height,nb_read_frames,r_frame_rate,sample_rate,channels",
                "-of",
                "default=noprint_wrappers=1",
            ])
            .arg(&path)
            .output()
            .unwrap();
        assert!(probe.status.success(), "{}", String::from_utf8_lossy(&probe.stderr));
        let metadata = String::from_utf8(probe.stdout).unwrap();
        for entry in
            ["codec_name=h264", "width=640", "height=360", "r_frame_rate=120/1", "nb_read_frames=120", "codec_name=aac", "sample_rate=48000", "channels=2"]
        {
            assert!(metadata.contains(entry), "missing {entry}: {metadata}");
        }
        let video = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args(["-map", "0:v:0", "-pix_fmt", "rgb24", "-f", "rawvideo", "pipe:1"])
            .output()
            .unwrap();
        assert!(video.status.success(), "{}", String::from_utf8_lossy(&video.stderr));
        assert_eq!(video.stdout.len(), 120 * 640 * 360 * 3);
        let center = (180 * 640 + 320) * 3;
        let first = &video.stdout[center..center + 3];
        let last_offset = 119 * 640 * 360 * 3 + center;
        let last = &video.stdout[last_offset..last_offset + 3];
        assert!(first[2] > first[0] + 50, "first frame blue background: {first:?}");
        assert!(last[0] > 230 && last[1] < 25 && last[2] < 25, "last frame red solid: {last:?}");
        let audio = Command::new("ffmpeg").args(["-v", "error", "-i"]).arg(&path).args(["-map", "0:a:0", "-f", "f32le", "pipe:1"]).output().unwrap();
        assert!(audio.status.success(), "{}", String::from_utf8_lossy(&audio.stderr));
        let samples: Vec<f32> = audio.stdout.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
        assert!((48_000..=49_024).contains(&(samples.len() / 2)));
        for (channel, hz) in [440.0_f32, 880.0].into_iter().enumerate() {
            let mut signal = 0_f64;
            let mut error = 0_f64;
            for position in 2048..45_056 {
                let expected = (std::f32::consts::TAU * hz * position as f32 / 48_000.0).sin() * 0.25;
                let actual = samples[position * 2 + channel];
                signal += f64::from(expected).powi(2);
                error += f64::from(actual - expected).powi(2);
            }
            assert!(10.0 * (signal / error).log10() > 30.0, "stereo tone channel {channel} changed or shifted");
        }
        let original = b"previous completed export";
        std::fs::write(&path, original).unwrap();
        assert!(matches!(export_fixture(&path, true), Err(ExportError::Cancelled)));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1, "native provisional export remains");
        std::fs::remove_dir_all(directory).unwrap();
    }
}
