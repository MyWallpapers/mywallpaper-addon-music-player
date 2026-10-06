#[cfg(windows)]
use std::io;

#[cfg(windows)]
use serde_json::{Map, Value, json};

#[cfg(windows)]
use crate::protocol::{read_json_record, write_json_record};

#[cfg(windows)]
const STATE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

pub fn run() -> Result<(), String> {
    #[cfg(windows)]
    {
        windows_runtime::run()
    }
    #[cfg(not(windows))]
    {
        Err("the music player companion is supported only on Windows".to_owned())
    }
}

#[cfg(windows)]
mod windows_runtime {
    use super::*;
    use base64::Engine;
    use std::collections::HashMap;
    use std::io::Write;
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
    use std::thread;
    use std::time::Instant;
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSession,
        GlobalSystemMediaTransportControlsSessionManager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus,
    };
    use windows::Media::MediaPlaybackAutoRepeatMode;
    use windows::Storage::Streams::DataReader;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
    use windows_future::IAsyncOperation;

    const ARTWORK_INPUT_LIMIT: u64 = 8 * 1024 * 1024;
    const ARTWORK_OUTPUT_LIMIT: usize = 180 * 1024;
    const MAX_ARTWORK_EDGE: u32 = 512;

    enum InputEvent {
        Frame(Value),
        Closed,
        Failed(String),
    }

    struct ComApartment;

    impl ComApartment {
        fn initialize() -> Result<Self, String> {
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
                .ok()
                .map_err(|error| {
                    format!("Could not initialize the Windows media session: {error}")
                })?;
            Ok(Self)
        }
    }

    impl Drop for ComApartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    pub fn run() -> Result<(), String> {
        let stdin = io::stdin();
        let mut reader = stdin.lock();
        let init = read_json_record(&mut reader, None)
            .map_err(|error| format!("Could not read host initialization: {error}"))?
            .ok_or_else(|| "host closed before initialization".to_owned())?;
        let version = init
            .get("v")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| "host initialization has no protocol version".to_owned())?;
        if version != 4 && version != 5 {
            let mut stdout = io::stdout().lock();
            let _ = write_json_record(
                &mut stdout,
                5,
                &json!({"type":"error", "v":version, "message":"Unsupported companion protocol version."}),
            );
            return Err(format!("unsupported host protocol version {version}"));
        }
        if init.get("type").and_then(Value::as_str) != Some("init") {
            return Err("first host record was not init".to_owned());
        }

        let mut stdout = io::stdout().lock();
        write_json_record(&mut stdout, version, &json!({"type":"ready", "v":version}))
            .map_err(|error| format!("Could not acknowledge initialization: {error}"))?;
        drop(reader);

        let (input_tx, input_rx) = mpsc::channel();
        spawn_input_reader(version, input_tx);
        let _apartment = ComApartment::initialize().ok();
        let mut controller = match _apartment.as_ref() {
            Some(_) => MediaController::new().ok(),
            None => None,
        };
        let mut media_unavailable = if _apartment.is_none() {
            Some("Windows media session access could not be initialized.".to_owned())
        } else if controller.is_none() {
            Some("Windows did not provide a media session manager.".to_owned())
        } else {
            None
        };

        let mut layout = read_layout(&init);
        let mut spectrum_monitor = None;
        let mut spectrum_frames = None;
        let mut spectrum_error_sent = false;
        if layout == "visualizer" {
            start_spectrum(
                &mut stdout,
                version,
                &mut spectrum_monitor,
                &mut spectrum_frames,
                &mut spectrum_error_sent,
            );
        }

        let mut next_state = Instant::now();
        let mut running = true;
        while running {
            match input_rx.recv_timeout(std::time::Duration::from_millis(25)) {
                Ok(InputEvent::Frame(frame)) => {
                    if frame.get("v").and_then(Value::as_u64) != Some(u64::from(version)) {
                        send_protocol_error(
                            &mut stdout,
                            version,
                            "Host record protocol version changed during the session.",
                        )?;
                        break;
                    }
                    match frame.get("type").and_then(Value::as_str) {
                        Some("settings") => {
                            let new_layout = read_layout(&frame);
                            if new_layout != layout {
                                layout = new_layout;
                                if layout == "visualizer" {
                                    spectrum_error_sent = false;
                                    start_spectrum(
                                        &mut stdout,
                                        version,
                                        &mut spectrum_monitor,
                                        &mut spectrum_frames,
                                        &mut spectrum_error_sent,
                                    );
                                } else {
                                    spectrum_frames = None;
                                    if let Some(monitor) = spectrum_monitor.take() {
                                        monitor.stop();
                                    }
                                    spectrum_error_sent = false;
                                }
                            }
                        }
                        Some("message") => {
                            if let Some(payload) = frame.get("payload") {
                                if payload.get("kind").and_then(Value::as_str)
                                    == Some("media.command")
                                {
                                    let request_id = payload
                                        .get("requestId")
                                        .and_then(Value::as_str)
                                        .unwrap_or("");
                                    let result = controller
                                        .as_mut()
                                        .ok_or_else(|| {
                                            media_unavailable.clone().unwrap_or_else(|| {
                                                "Windows media session access is unavailable."
                                                    .to_owned()
                                            })
                                        })
                                        .and_then(|media| media.command(payload));
                                    let (ok, message) = match result {
                                        Ok(message) => (true, message),
                                        Err(message) => (false, message),
                                    };
                                    send_message(
                                        &mut stdout,
                                        version,
                                        json!({
                                            "kind":"media.result",
                                            "requestId":request_id,
                                            "ok":ok,
                                            "message":message
                                        }),
                                    )?;
                                    next_state = Instant::now();
                                }
                            }
                        }
                        Some("shutdown") => running = false,
                        Some(_) | None => {}
                    }
                }
                Ok(InputEvent::Closed) => running = false,
                Ok(InputEvent::Failed(message)) => {
                    send_protocol_error(&mut stdout, version, &message)?;
                    running = false;
                }
                Err(RecvTimeoutError::Disconnected) => running = false,
                Err(RecvTimeoutError::Timeout) => {}
            }

            if !running {
                break;
            }

            if let Some(frames) = spectrum_frames.as_ref() {
                loop {
                    match frames.try_recv() {
                        Ok(frame) => send_message(
                            &mut stdout,
                            version,
                            json!({"kind":"media.spectrum", "bands":frame.bands}),
                        )?,
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            spectrum_frames = None;
                            spectrum_monitor = None;
                            if !spectrum_error_sent {
                                send_message(
                                    &mut stdout,
                                    version,
                                    json!({
                                        "kind":"media.spectrum.error",
                                        "message":"Windows audio capture stopped because the render endpoint was disconnected."
                                    }),
                                )?;
                                spectrum_error_sent = true;
                            }
                            break;
                        }
                    }
                }
            }

            if Instant::now() >= next_state {
                let state = match controller.as_mut() {
                    Some(media) => {
                        media_unavailable = None;
                        media.snapshot()
                    }
                    None => json!({
                        "kind":"media.state",
                        "status":"unavailable",
                        "sessions":[],
                        "message":media_unavailable.as_deref().unwrap_or("Windows media session access is unavailable.")
                    }),
                };
                send_message(&mut stdout, version, state)?;
                next_state = Instant::now() + STATE_INTERVAL;
            }
        }

        if let Some(monitor) = spectrum_monitor.take() {
            monitor.stop();
        }
        Ok(())
    }

    fn spawn_input_reader(version: u32, sender: Sender<InputEvent>) {
        thread::Builder::new()
            .name("music-player-host-input".to_owned())
            .spawn(move || {
                let stdin = io::stdin();
                let mut reader = stdin.lock();
                loop {
                    match read_json_record(&mut reader, Some(version)) {
                        Ok(Some(frame)) => {
                            if sender.send(InputEvent::Frame(frame)).is_err() {
                                return;
                            }
                        }
                        Ok(None) => {
                            let _ = sender.send(InputEvent::Closed);
                            return;
                        }
                        Err(error) => {
                            let _ = sender
                                .send(InputEvent::Failed(format!("Invalid host record: {error}")));
                            return;
                        }
                    }
                }
            })
            .ok();
    }

    fn read_layout(frame: &Value) -> String {
        frame
            .get("layerSettings")
            .and_then(|settings| settings.get("layout"))
            .and_then(Value::as_str)
            .unwrap_or("minimal")
            .to_owned()
    }

    fn send_message<W: Write>(writer: &mut W, version: u32, payload: Value) -> Result<(), String> {
        write_json_record(
            writer,
            version,
            &json!({"type":"message", "v":version, "target":"broadcast", "payload":payload}),
        )
        .map_err(|error| format!("Could not send media state: {error}"))
    }

    fn send_protocol_error<W: Write>(
        writer: &mut W,
        version: u32,
        message: &str,
    ) -> Result<(), String> {
        write_json_record(
            writer,
            version,
            &json!({"type":"error", "v":version, "message":message}),
        )
        .map_err(|error| format!("Could not report companion protocol error: {error}"))
    }

    fn start_spectrum<W: Write>(
        writer: &mut W,
        version: u32,
        monitor: &mut Option<crate::audio::SpectrumMonitor>,
        frames: &mut Option<Receiver<crate::audio::SpectrumFrame>>,
        error_sent: &mut bool,
    ) {
        match crate::audio::start_spectrum_monitor() {
            Ok((started, receiver)) => {
                *monitor = Some(started);
                *frames = Some(receiver);
                *error_sent = false;
            }
            Err(error) => {
                *frames = None;
                *monitor = None;
                if !*error_sent {
                    let _ = send_message(
                        writer,
                        version,
                        json!({"kind":"media.spectrum.error", "message":error.to_string()}),
                    );
                    *error_sent = true;
                }
            }
        }
    }

    struct SessionItem {
        id: String,
        source: String,
        session: GlobalSystemMediaTransportControlsSession,
    }

    struct MediaController {
        manager: GlobalSystemMediaTransportControlsSessionManager,
        active_session: Option<String>,
        sessions: HashMap<String, GlobalSystemMediaTransportControlsSession>,
        last_track_signature: Option<String>,
    }

    impl MediaController {
        fn new() -> Result<Self, String> {
            let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
                .map_err(|error| format!("Could not request Windows media sessions: {error}"))?
                .join()
                .map_err(|error| format!("Could not connect to Windows media sessions: {error}"))?;
            Ok(Self {
                manager,
                active_session: None,
                sessions: HashMap::new(),
                last_track_signature: None,
            })
        }

        fn session_items(&mut self) -> Result<Vec<SessionItem>, String> {
            let sessions = self
                .manager
                .GetSessions()
                .map_err(|error| format!("Could not enumerate Windows media sessions: {error}"))?;
            let current_source = self
                .manager
                .GetCurrentSession()
                .ok()
                .and_then(|session| session.SourceAppUserModelId().ok());
            let mut source_indices: HashMap<String, usize> = HashMap::new();
            let mut items = Vec::new();
            self.sessions.clear();
            for index in 0..sessions.Size().unwrap_or(0) {
                let Ok(session) = sessions.GetAt(index) else {
                    continue;
                };
                let source = session
                    .SourceAppUserModelId()
                    .map(|value| value.to_string())
                    .unwrap_or_else(|_| "unknown player".to_owned());
                let source_index = source_indices.entry(source.clone()).or_default();
                let id = format!("{}#{}", source, *source_index);
                *source_index += 1;
                self.sessions.insert(id.clone(), session.clone());
                items.push(SessionItem {
                    id,
                    source,
                    session,
                });
            }
            if self
                .active_session
                .as_ref()
                .is_some_and(|active| !self.sessions.contains_key(active))
            {
                self.active_session = None;
            }
            if self.active_session.is_none() {
                if let Some(current_source) = current_source {
                    self.active_session = items
                        .iter()
                        .find(|item| item.source == current_source)
                        .map(|item| item.id.clone());
                }
                if self.active_session.is_none() {
                    self.active_session = items.first().map(|item| item.id.clone());
                }
            }
            Ok(items)
        }

        fn snapshot(&mut self) -> Value {
            let items = match self.session_items() {
                Ok(items) => items,
                Err(message) => {
                    return json!({
                        "kind":"media.state",
                        "status":"unavailable",
                        "sessions":[],
                        "message":message
                    });
                }
            };
            let session_list = items
                .iter()
                .map(|item| json!({"id":item.id, "label":display_source(&item.source)}))
                .collect::<Vec<_>>();
            let Some(selected) = self
                .active_session
                .as_ref()
                .and_then(|id| items.iter().find(|item| &item.id == id))
                .or_else(|| items.first())
            else {
                self.last_track_signature = None;
                return json!({"kind":"media.state", "status":"empty", "sessions":session_list});
            };

            let playback = selected.session.GetPlaybackInfo().ok();
            let controls = playback.as_ref().and_then(|info| info.Controls().ok());
            let is_playing = playback
                .as_ref()
                .and_then(|info| info.PlaybackStatus().ok())
                .is_some_and(|status| {
                    status == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing
                });
            let properties = selected
                .session
                .TryGetMediaPropertiesAsync()
                .ok()
                .and_then(|operation| operation.join().ok());
            let title = properties
                .as_ref()
                .and_then(|properties| properties.Title().ok())
                .map(|value| value.to_string());
            let artist = properties
                .as_ref()
                .and_then(|properties| properties.Artist().ok())
                .map(|value| value.to_string());
            let album = properties
                .as_ref()
                .and_then(|properties| properties.AlbumTitle().ok())
                .map(|value| value.to_string());
            let signature = format!(
                "{}\u{1f}{}\u{1f}{}\u{1f}{}",
                selected.id,
                title.as_deref().unwrap_or(""),
                artist.as_deref().unwrap_or(""),
                album.as_deref().unwrap_or("")
            );
            let track_changed = self.last_track_signature.as_deref() != Some(signature.as_str());
            let timeline = selected.session.GetTimelineProperties().ok();
            let mut state = Map::new();
            state.insert("kind".to_owned(), json!("media.state"));
            state.insert("status".to_owned(), json!("ready"));
            state.insert("sessionId".to_owned(), json!(selected.id));
            state.insert("source".to_owned(), json!(display_source(&selected.source)));
            state.insert("sessions".to_owned(), json!(session_list));
            state.insert("title".to_owned(), json!(title.unwrap_or_default()));
            state.insert("artist".to_owned(), json!(artist.unwrap_or_default()));
            state.insert("album".to_owned(), json!(album.unwrap_or_default()));
            state.insert("playing".to_owned(), json!(is_playing));
            state.insert(
                "shuffle".to_owned(),
                json!(
                    playback
                        .as_ref()
                        .and_then(|info| info.IsShuffleActive().ok())
                        .and_then(|value| value.Value().ok())
                        .unwrap_or(false)
                ),
            );
            let repeat = playback
                .as_ref()
                .and_then(|info| info.AutoRepeatMode().ok())
                .and_then(|mode| mode.Value().ok())
                .map(repeat_name)
                .unwrap_or("none");
            state.insert("repeat".to_owned(), json!(repeat));
            state.insert(
                "capabilities".to_owned(),
                json!({
                    "playPause":controls.as_ref().is_some_and(|control| control.IsPlayPauseToggleEnabled().unwrap_or(false) || if is_playing { control.IsPauseEnabled().unwrap_or(false) } else { control.IsPlayEnabled().unwrap_or(false) }),
                    "previous":controls.as_ref().is_some_and(|control| control.IsPreviousEnabled().unwrap_or(false)),
                    "next":controls.as_ref().is_some_and(|control| control.IsNextEnabled().unwrap_or(false)),
                    "seek":controls.as_ref().is_some_and(|control| control.IsPlaybackPositionEnabled().unwrap_or(false)),
                    "shuffle":controls.as_ref().is_some_and(|control| control.IsShuffleEnabled().unwrap_or(false)),
                    "repeat":controls.as_ref().is_some_and(|control| control.IsRepeatEnabled().unwrap_or(false)),
                }),
            );
            if let Some(timeline) = timeline {
                let start_ms = timeline
                    .MinSeekTime()
                    .map(|value| ticks_to_ms(value.Duration))
                    .unwrap_or(0);
                let end_ms = timeline
                    .MaxSeekTime()
                    .map(|value| ticks_to_ms(value.Duration))
                    .unwrap_or(start_ms);
                let duration_ms = timeline
                    .EndTime()
                    .map(|value| ticks_to_ms(value.Duration))
                    .unwrap_or(0);
                let raw_position_ms = timeline
                    .Position()
                    .map(|value| ticks_to_ms(value.Duration))
                    .unwrap_or(0);
                let position_ms = if is_playing {
                    let elapsed = timeline
                        .LastUpdatedTime()
                        .ok()
                        .map(|updated| elapsed_since_windows_time(updated.UniversalTime))
                        .unwrap_or(0);
                    raw_position_ms
                        .saturating_add(elapsed)
                        .min(end_ms.max(duration_ms).max(raw_position_ms))
                } else {
                    raw_position_ms
                };
                state.insert("positionMs".to_owned(), json!(position_ms));
                state.insert("durationMs".to_owned(), json!(duration_ms));
                state.insert("seekStartMs".to_owned(), json!(start_ms));
                state.insert("seekEndMs".to_owned(), json!(end_ms.max(start_ms)));
            }
            if track_changed {
                let artwork = properties
                    .as_ref()
                    .and_then(|properties| properties.Thumbnail().ok())
                    .and_then(|thumbnail| load_artwork(thumbnail).ok().flatten());
                state.insert("artwork".to_owned(), json!(artwork));
                self.last_track_signature = Some(signature);
            }
            Value::Object(state)
        }

        fn command(&mut self, payload: &Value) -> Result<String, String> {
            let action = payload
                .get("action")
                .and_then(Value::as_str)
                .ok_or_else(|| "Media command has no action.".to_owned())?;
            if action == "refresh" {
                return Ok("Media state refreshed.".to_owned());
            }
            if action == "session" {
                let id = payload
                    .get("value")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "Session command has no session id.".to_owned())?;
                if !self.sessions.contains_key(id) {
                    return Err("That media session is no longer available.".to_owned());
                }
                self.active_session = Some(id.to_owned());
                self.last_track_signature = None;
                return Ok("Media session selected.".to_owned());
            }
            let id = self
                .active_session
                .as_ref()
                .ok_or_else(|| "There is no active Windows media session.".to_owned())?;
            let session = self
                .sessions
                .get(id)
                .ok_or_else(|| "The active Windows media session is unavailable.".to_owned())?;
            let playback = session
                .GetPlaybackInfo()
                .map_err(|error| format!("Could not read player controls: {error}"))?;
            let controls = playback
                .Controls()
                .map_err(|error| format!("Could not read player controls: {error}"))?;
            match action {
                "playPause" => {
                    if controls.IsPlayPauseToggleEnabled().unwrap_or(false) {
                        try_bool(session.TryTogglePlayPauseAsync(), "toggle play/pause")?;
                    } else {
                        let playing = playback
                            .PlaybackStatus()
                            .is_ok_and(|status| status == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing);
                        if playing && controls.IsPauseEnabled().unwrap_or(false) {
                            try_bool(session.TryPauseAsync(), "pause")?;
                        } else if !playing && controls.IsPlayEnabled().unwrap_or(false) {
                            try_bool(session.TryPlayAsync(), "play")?;
                        } else {
                            return Err("This player does not support play/pause.".to_owned());
                        }
                    }
                }
                "previous" => {
                    require_control(
                        controls.IsPreviousEnabled().unwrap_or(false),
                        "previous track",
                    )?;
                    try_bool(session.TrySkipPreviousAsync(), "previous track")?;
                }
                "next" => {
                    require_control(controls.IsNextEnabled().unwrap_or(false), "next track")?;
                    try_bool(session.TrySkipNextAsync(), "next track")?;
                }
                "seek" => {
                    require_control(
                        controls.IsPlaybackPositionEnabled().unwrap_or(false),
                        "seeking",
                    )?;
                    let requested_ms =
                        payload
                            .get("value")
                            .and_then(Value::as_i64)
                            .ok_or_else(|| {
                                "Seek command requires an absolute position in milliseconds."
                                    .to_owned()
                            })?;
                    let timeline = session
                        .GetTimelineProperties()
                        .map_err(|error| format!("Could not read the seek range: {error}"))?;
                    let lower = timeline
                        .MinSeekTime()
                        .map(|value| ticks_to_ms(value.Duration))
                        .unwrap_or(0);
                    let upper = timeline
                        .MaxSeekTime()
                        .map(|value| ticks_to_ms(value.Duration))
                        .unwrap_or(lower)
                        .max(lower);
                    let target_ms = requested_ms.clamp(lower, upper);
                    try_bool(
                        session.TryChangePlaybackPositionAsync(target_ms.saturating_mul(10_000)),
                        "seeking",
                    )?;
                }
                "shuffle" => {
                    require_control(controls.IsShuffleEnabled().unwrap_or(false), "shuffle")?;
                    let enabled = payload
                        .get("value")
                        .and_then(Value::as_bool)
                        .ok_or_else(|| "Shuffle command requires a boolean value.".to_owned())?;
                    try_bool(session.TryChangeShuffleActiveAsync(enabled), "shuffle")?;
                }
                "repeat" => {
                    require_control(controls.IsRepeatEnabled().unwrap_or(false), "repeat")?;
                    let repeat = match payload.get("value").and_then(Value::as_str) {
                        Some("none") => MediaPlaybackAutoRepeatMode::None,
                        Some("track") => MediaPlaybackAutoRepeatMode::Track,
                        Some("list") => MediaPlaybackAutoRepeatMode::List,
                        _ => return Err("Repeat command requires none, track, or list.".to_owned()),
                    };
                    try_bool(session.TryChangeAutoRepeatModeAsync(repeat), "repeat mode")?;
                }
                _ => return Err(format!("Unsupported media command: {action}")),
            }
            Ok("Command sent to the selected Windows media session.".to_owned())
        }
    }

    fn require_control(enabled: bool, label: &str) -> Result<(), String> {
        if enabled {
            Ok(())
        } else {
            Err(format!("This player does not support {label}."))
        }
    }

    fn try_bool(
        operation: windows::core::Result<IAsyncOperation<bool>>,
        action: &str,
    ) -> Result<(), String> {
        let operation = operation
            .map_err(|error| format!("Windows could not send the {action} command: {error}"))?;
        let accepted = operation
            .join()
            .map_err(|error| format!("Windows could not send the {action} command: {error}"))?;
        if accepted {
            Ok(())
        } else {
            Err(format!(
                "The selected player declined the {action} command."
            ))
        }
    }

    fn repeat_name(mode: MediaPlaybackAutoRepeatMode) -> &'static str {
        match mode {
            MediaPlaybackAutoRepeatMode::Track => "track",
            MediaPlaybackAutoRepeatMode::List => "list",
            _ => "none",
        }
    }

    fn ticks_to_ms(time: i64) -> i64 {
        (time / 10_000).max(0)
    }

    fn elapsed_since_windows_time(updated: i64) -> i64 {
        const WINDOWS_TO_UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| {
                duration.as_secs() as i64 * 10_000_000
                    + i64::from(duration.subsec_nanos() / 100)
                    + WINDOWS_TO_UNIX_EPOCH_TICKS
            })
            .unwrap_or(WINDOWS_TO_UNIX_EPOCH_TICKS);
        now.saturating_sub(updated).max(0) / 10_000
    }

    fn display_source(source: &str) -> String {
        let app = source.rsplit('!').next().unwrap_or(source);
        let app = app.rsplit(['\\', '/']).next().unwrap_or(app);
        app.strip_suffix(".exe").unwrap_or(app).to_owned()
    }

    fn load_artwork(
        thumbnail: windows::Storage::Streams::IRandomAccessStreamReference,
    ) -> Result<Option<String>, String> {
        let stream = thumbnail
            .OpenReadAsync()
            .map_err(|error| format!("Could not open album artwork: {error}"))?
            .join()
            .map_err(|error| format!("Could not read album artwork: {error}"))?;
        let size = stream
            .Size()
            .map_err(|error| format!("Could not read album artwork size: {error}"))?;
        if size == 0 || size > ARTWORK_INPUT_LIMIT {
            return Ok(None);
        }
        let input = stream
            .GetInputStreamAt(0)
            .map_err(|error| format!("Could not open album artwork stream: {error}"))?;
        let reader = DataReader::CreateDataReader(&input)
            .map_err(|error| format!("Could not read album artwork stream: {error}"))?;
        let read = reader
            .LoadAsync(size as u32)
            .map_err(|error| format!("Could not load album artwork: {error}"))?
            .join()
            .map_err(|error| format!("Could not load album artwork: {error}"))?;
        let mut bytes = vec![0_u8; read as usize];
        reader
            .ReadBytes(&mut bytes)
            .map_err(|error| format!("Could not copy album artwork: {error}"))?;
        encode_artwork(&bytes)
    }

    fn encode_artwork(bytes: &[u8]) -> Result<Option<String>, String> {
        use image::ImageReader;
        use std::io::Cursor;

        if bytes.is_empty() {
            return Ok(None);
        }
        let Ok(reader) = ImageReader::new(Cursor::new(bytes)).with_guessed_format() else {
            return Ok(None);
        };
        let Ok(image) = reader.decode() else {
            return Ok(None);
        };
        let image = image
            .thumbnail(MAX_ARTWORK_EDGE, MAX_ARTWORK_EDGE)
            .to_rgb8();
        let mut encoded = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 78)
            .encode_image(&image)
            .map_err(|error| format!("Could not encode album artwork: {error}"))?;
        if encoded.len() > ARTWORK_OUTPUT_LIMIT {
            encoded.clear();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 58)
                .encode_image(&image)
                .map_err(|error| format!("Could not compress album artwork: {error}"))?;
        }
        if encoded.len() > ARTWORK_OUTPUT_LIMIT {
            return Ok(None);
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(encoded);
        Ok(Some(format!("data:image/jpeg;base64,{encoded}")))
    }
}
