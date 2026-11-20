//! Voice support via LiveKit.

use std::sync::Arc;
use livekit::options::TrackPublishOptions;
use livekit::track::{LocalAudioTrack, LocalTrack, RemoteTrack, TrackSource};
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use livekit::webrtc::prelude::*;
use livekit::{Room, RoomEvent};
use std::process::Stdio;
use tokio::io::AsyncReadExt as _;
use tokio::process::Command;
use crate::http::Http;
use tokio::task::AbortHandle;
use futures::StreamExt as _;
use crate::client::Context;

/// A single audio frame received from a remote voice participant.
#[derive(Debug, Clone)]
pub struct VoiceFrame {
    pub participant_identity: String,
    pub data: Vec<i16>,
    pub sample_rate: u32,
    pub num_channels: u32,
    pub samples_per_channel: u32,
}

/// A voice connection backed by LiveKit. Get one from [`Context::join_voice`](crate::client::Context::join_voice).
pub struct FluxerVoiceConnection {
    /// The underlying LiveKit room, exposed in case you need it for anything advanced.
    pub room: Arc<Room>,
    audio_source: NativeAudioSource,
}

impl FluxerVoiceConnection {
    /// Connects to a LiveKit voice server. Called internally by
    /// [`Context::join_voice`](crate::client::Context::join_voice).
    pub async fn connect(
        url: &str,
        token: &str,
        ctx: Context,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let (room, mut events) = Room::connect(url, token, Default::default()).await?;
        let room = Arc::new(room);
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if let RoomEvent::TrackSubscribed { track: RemoteTrack::Audio(audio_track), participant, .. } = event {
                    let ctx = ctx.clone();
                    let identity = participant.identity().to_string();
                    let rtc_track = audio_track.rtc_track();
                    tokio::spawn(async move {
                        let mut stream = NativeAudioStream::new(rtc_track, 48_000, 2);
                        while let Some(frame) = stream.next().await {
                            ctx.handler
                                .on_voice_receive(
                                    ctx.clone(),
                                    VoiceFrame {
                                        participant_identity: identity.clone(),
                                        data: frame.data.to_vec(),
                                        sample_rate: frame.sample_rate,
                                        num_channels: frame.num_channels,
                                        samples_per_channel: frame.samples_per_channel,
                                    },
                                )
                                .await;
                        }
                    });
                }
            }
        });
        let source = NativeAudioSource::new(Default::default(), 48_000, 2, 960);

        let track = LocalAudioTrack::create_audio_track(
            "audio",
            livekit::webrtc::audio_source::RtcAudioSource::Native(source.clone()),
        );

        room.local_participant()
            .publish_track(
                LocalTrack::Audio(track),
                TrackPublishOptions {
                    source: TrackSource::Microphone,
                    ..Default::default()
                },
            )
            .await?;

        Ok(Self { room, audio_source: source })
    }

    /// Plays audio from a file (anything ffmpeg can decode). Spawns ffmpeg
    /// in the background and streams PCM into the voice channel.
    ///
    /// Returns an [`AbortHandle`] you can call `.abort()` on to stop playback.
    /// If ffmpeg errors out, the last few lines of stderr get sent to `channel_id`.
    /// botsu patch: plays `input` (a file or URL; `"pipe:0"` with `stdin` set to read from
    /// another process, such as `yt-dlp -o -`). Unlike [`play_music`](Self::play_music): network
    /// inputs get reconnect flags, ffmpeg's stderr is drained while playing (a full pipe would
    /// stall it), errors are returned from the task instead of posted to a channel, and setting
    /// `paused` holds playback (ffmpeg then blocks on its full output pipe). `volume` is in
    /// percent (100 = unchanged), read for every frame so changes apply right away.
    pub fn play_input(
        &self,
        input: &str,
        stdin: Option<Stdio>,
        paused: Arc<std::sync::atomic::AtomicBool>,
        volume: Arc<std::sync::atomic::AtomicU16>,
    ) -> Result<tokio::task::JoinHandle<Result<(), String>>, Box<dyn std::error::Error + Send + Sync>>
    {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-loglevel", "error", "-re"]);
        if input.starts_with("http://") || input.starts_with("https://") {
            cmd.args([
                "-reconnect",
                "1",
                "-reconnect_streamed",
                "1",
                "-reconnect_delay_max",
                "5",
            ]);
        }
        cmd.args(["-i", input, "-f", "s16le", "-ar", "48000", "-ac", "2", "pipe:1"])
            .stdin(stdin.unwrap_or_else(Stdio::null))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn()?;
        let mut stdout = child.stdout.take().ok_or("ffmpeg: no stdout")?;
        let mut stderr = child.stderr.take().ok_or("ffmpeg: no stderr")?;
        let errors = tokio::spawn(async move {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text).await;
            text
        });
        let source = self.audio_source.clone();

        Ok(tokio::spawn(async move {
            let mut buffer = vec![0u8; 960 * 2 * 2];
            let mut failure: Option<String> = None;
            loop {
                while paused.load(std::sync::atomic::Ordering::Relaxed) {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                match stdout.read_exact(&mut buffer).await {
                    Ok(_) => {
                        let gain =
                            i32::from(volume.load(std::sync::atomic::Ordering::Relaxed));
                        let samples: Vec<i16> = buffer
                            .chunks_exact(2)
                            .map(|c| i16::from_le_bytes([c[0], c[1]]))
                            .map(|s| {
                                if gain == 100 {
                                    s
                                } else {
                                    (i32::from(s) * gain / 100)
                                        .clamp(i32::from(i16::MIN), i32::from(i16::MAX))
                                        as i16
                                }
                            })
                            .collect();
                        if let Err(e) = source
                            .capture_frame(&AudioFrame {
                                data: samples.into(),
                                num_channels: 2,
                                sample_rate: 48_000,
                                samples_per_channel: 960,
                            })
                            .await
                        {
                            failure = Some(format!("audio capture error: {e}"));
                            break;
                        }
                    }
                    Err(e) => {
                        if e.kind() != std::io::ErrorKind::UnexpectedEof {
                            failure = Some(format!("PCM read error: {e}"));
                        }
                        break;
                    }
                }
            }
            let status = child.wait().await;
            let stderr_text = errors.await.unwrap_or_default();
            let ok = status.map(|s| s.success()).unwrap_or(false);
            match (failure, ok) {
                (None, true) => Ok(()),
                (Some(f), _) => Err(f),
                (None, false) => Err(format!(
                    "ffmpeg failed: {}",
                    stderr_text.trim().lines().last().unwrap_or("no output")
                )),
            }
        }))
    }

    pub async fn play_music(
        &self,
        path: &str,
        http: Arc<Http>,
        channel_id: String,
    ) -> Result<AbortHandle, Box<dyn std::error::Error + Send + Sync>> {
        let mut child = Command::new("ffmpeg")
            .args(["-re", "-i", path, "-f", "s16le", "-ar", "48000", "-ac", "2", "pipe:1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let mut stdout = child.stdout.take().ok_or("ffmpeg: no stdout")?;
        let mut stderr = child.stderr.take().ok_or("ffmpeg: no stderr")?;
        let source = self.audio_source.clone();

        let handle = tokio::spawn(async move {
            let mut buffer = vec![0u8; 960 * 2 * 2];
            let mut stream_error: Option<String> = None;

            loop {
                match stdout.read_exact(&mut buffer).await {
                    Ok(_) => {
                        let samples: Vec<i16> = buffer
                            .chunks_exact(2)
                            .map(|c| i16::from_le_bytes([c[0], c[1]]))
                            .collect();

                        if let Err(e) = source.capture_frame(&AudioFrame {
                            data: samples.into(),
                            num_channels: 2,
                            sample_rate: 48_000,
                            samples_per_channel: 960,
                        }).await {
                            stream_error = Some(format!("Audio capture error: {}", e));
                            break;
                        }
                    }
                    Err(e) => {
                        if e.kind() != std::io::ErrorKind::UnexpectedEof {
                            stream_error = Some(format!("PCM read error: {}", e));
                        }
                        break;
                    }
                }
            }

            let exit_status = child.wait().await;
            let failed = exit_status.map(|s| !s.success()).unwrap_or(true);
            if failed || stream_error.is_some() {
                let mut stderr_output = String::new();
                let _ = stderr.read_to_string(&mut stderr_output).await;

                let last_lines: String = stderr_output
                    .lines()
                    .rev()
                    .take(3)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n");

                let error_msg = stream_error.unwrap_or_else(|| {
                    format!("ffmpeg exited with an error:\n```\n{}\n```", last_lines)
                });

                let _ = http.send_message(&channel_id, &error_msg).await;
            }
        });

        Ok(handle.abort_handle())
    }
}