use super::StreamInfo;
use dash_mpd::{MPD, Representation, S};
use log::{error, info};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tempfile::tempdir;
use url::Url;

#[allow(unused)]
struct LiveHlsPusher {
    child: Child,
    ffmpeg_stdin: ChildStdin,
}

impl LiveHlsPusher {
    pub fn spawn(output_dir: &str, max_segments: u32, segment_time: u32) -> anyhow::Result<Self> {
        let mut child = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-i",
                "pipe:0",
                "-c:v",
                "copy",
                "-c:a",
                "aac",
                "-ac",
                "2",
                "-channel_layout",
                "stereo",
                "-b:a",
                "128k",
                "-ar",
                "48000",
                "-f",
                "hls",
                "-hls_time",
                &segment_time.to_string(),
                "-hls_list_size",
                &max_segments.to_string(),
                "-hls_flags",
                "delete_segments",
                "-hls_segment_type",
                "mpegts",
                "-hls_segment_filename",
                &format!("{}/segment_%03d.ts", output_dir),
                &format!("{}/master.m3u8", output_dir),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;

        let ffmpeg_stdin = child.stdin.take().unwrap();

        let stderr = child.stderr.take().map(BufReader::new);
        // Spawn a thread to read stderr
        std::thread::spawn(move || {
            if let Some(mut reader) = stderr {
                let mut buf = String::new();
                while let Ok(n) = reader.read_line(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    error!("[ffmpeg] {}", buf.trim());
                    buf.clear();
                }
            }
        });

        Ok(Self {
            child,
            ffmpeg_stdin,
        })
    }

    pub fn write(&mut self, data: &[u8]) -> anyhow::Result<()> {
        self.ffmpeg_stdin.write_all(data)?;
        self.ffmpeg_stdin.flush()?;
        Ok(())
    }

    pub fn kill(&mut self) -> anyhow::Result<()> {
        match self.child.kill() {
            Ok(_) => Ok(()),
            Err(e) => {
                error!("Error killing ffmpeg pusher process: {}", e);
                Err(e.into())
            }
        }
    }
}

// DASH-to-HLS converter implementation
pub struct DashToHlsConverter {
    stream_info: StreamInfo,
    is_active: bool,
    sequence_number: u32,
    temp_dir: PathBuf,
    last_processed_segments: (Vec<String>, Vec<String>),
    pusher: LiveHlsPusher,
}

impl DashToHlsConverter {
    pub fn new(
        output_dir: &str,
        stream_info: StreamInfo,
        max_segments: u32,
        segment_duration: u32,
    ) -> io::Result<Self> {
        // Create output directory
        fs::create_dir_all(output_dir)?;

        let temp_dir = match tempdir() {
            Ok(dir) => dir.into_path(),
            Err(_) => {
                let path = PathBuf::from(format!("{}/temp", output_dir));
                fs::create_dir_all(&path)?;
                path
            }
        };

        let pusher = LiveHlsPusher::spawn(output_dir, max_segments, segment_duration).unwrap();

        Ok(Self {
            stream_info,
            is_active: false,
            sequence_number: 0,
            temp_dir,
            last_processed_segments: (Vec::new(), Vec::new()),
            pusher,
        })
    }

    fn start(&mut self) -> io::Result<()> {
        if self.is_active {
            return Ok(());
        }

        info!("Starting converter for stream: {}", self.stream_info.id);
        self.is_active = true;

        Ok(())
    }

    fn process_mpd(
        &self,
    ) -> anyhow::Result<((Vec<String>, Option<String>), (Vec<String>, Option<String>))> {
        // Parse the MPD
        let mpd_url = Url::parse(&self.stream_info.url)?;
        let client = reqwest::blocking::Client::builder()
            .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:133.0) Gecko/20100101 Firefox/133.0")
            .timeout(Duration::from_secs(30))
            .build()?;

        let mpd_response = client.get(mpd_url.clone()).send()?;

        if !mpd_response.status().is_success() {
            anyhow::bail!("Failed to fetch MPD: HTTP {}", mpd_response.status());
        }

        let mpd_content = mpd_response.text()?;
        let mpd = dash_mpd::parse(&mpd_content)?;

        // Find video and audio representations
        let mut video_segments = Vec::new();
        let mut audio_segments = Vec::new();
        let mut video_init = None;
        let mut audio_init = None;

        // Try to find representations at specific indices first
        // If that fails, look for highest quality video and any audio
        self.extract_segments_from_mpd(
            &mpd,
            &mpd_url,
            &mut video_segments,
            &mut audio_segments,
            &mut video_init,
            &mut audio_init,
        )?;

        Ok(((video_segments, video_init), (audio_segments, audio_init)))
    }

    fn extract_segments_from_mpd(
        &self,
        mpd: &MPD,
        mpd_url: &Url,
        video_segments: &mut Vec<String>,
        audio_segments: &mut Vec<String>,
        video_init: &mut Option<String>,
        audio_init: &mut Option<String>,
    ) -> anyhow::Result<()> {
        let mut video_rep_found = false;
        let mut audio_rep_found = false;

        // First try specific indices
        let video_index = 6;
        let audio_index = 9;

        for period in &mpd.periods {
            let mut rep_index = 0;

            for adaptation_set in &period.adaptations {
                for representation in &adaptation_set.representations {
                    if (adaptation_set.mimeType.as_deref() == Some("video/mp4")
                        || adaptation_set.contentType.as_deref() == Some("video"))
                        && rep_index == video_index
                    {
                        (*video_segments, *video_init) =
                            self.extract_segments(&mpd, &representation, &mpd_url)?;
                        video_rep_found = true;
                    } else if (adaptation_set.mimeType.as_deref() == Some("audio/mp4")
                        || adaptation_set.contentType.as_deref() == Some("audio"))
                        && rep_index == audio_index
                    {
                        (*audio_segments, *audio_init) =
                            self.extract_segments(&mpd, &representation, &mpd_url)?;
                        audio_rep_found = true;
                    }

                    rep_index += 1;
                }
            }
        }

        // If specific indices not found, try to use best available
        if !video_rep_found || !audio_rep_found {
            info!("Specific representation indices not found, using best available");

            for period in &mpd.periods {
                for adaptation_set in &period.adaptations {
                    // For video, get highest bandwidth representation
                    if (adaptation_set.mimeType.as_deref() == Some("video/mp4")
                        || adaptation_set.contentType.as_deref() == Some("video"))
                        && !video_rep_found
                    {
                        if let Some(rep) = adaptation_set
                            .representations
                            .iter()
                            .max_by_key(|r| r.bandwidth.unwrap_or(0))
                        {
                            info!(
                                "Selected video representation with bandwidth: {}",
                                rep.bandwidth.unwrap_or(0)
                            );
                            (*video_segments, *video_init) =
                                self.extract_segments(&mpd, rep, &mpd_url)?;
                            video_rep_found = true;
                        }
                    }
                    // For audio, get first available representation
                    else if (adaptation_set.mimeType.as_deref() == Some("audio/mp4")
                        || adaptation_set.contentType.as_deref() == Some("audio"))
                        && !audio_rep_found
                    {
                        if !adaptation_set.representations.is_empty() {
                            let rep = &adaptation_set.representations[0];
                            info!(
                                "Selected audio representation with bandwidth: {}",
                                rep.bandwidth.unwrap_or(0)
                            );
                            (*audio_segments, *audio_init) =
                                self.extract_segments(&mpd, rep, &mpd_url)?;
                            audio_rep_found = true;
                        }
                    }
                }
            }
        }

        if !video_rep_found {
            info!("No video representation found");
        }

        if !audio_rep_found {
            info!("No audio representation found");
        }

        Ok(())
    }

    fn extract_segments(
        &self,
        mpd: &MPD,
        representation: &Representation,
        base_url: &Url,
    ) -> anyhow::Result<(Vec<String>, Option<String>)> {
        let mut segments = Vec::new();
        let mut init_segment = None;
        let mut base_url_str = base_url.to_string();

        // Check if we have BaseURL at MPD level
        if let Some(mpd_base_url) = representation.BaseURL.first() {
            base_url_str = mpd_base_url.base.clone();
        }

        // Check if period has BaseURL
        let period = mpd
            .periods
            .first()
            .ok_or_else(|| anyhow::anyhow!("No period found"))?;
        if let Some(period_base_url) = period.BaseURL.first() {
            base_url_str = if period_base_url.base.starts_with("http") {
                period_base_url.base.clone()
            } else {
                format!(
                    "{}/{}",
                    base_url_str.trim_end_matches('/'),
                    period_base_url.base
                )
            };
        }

        // Handle representation BaseURL
        if let Some(rep_base_url) = representation.BaseURL.first() {
            base_url_str = if rep_base_url.base.starts_with("http") {
                rep_base_url.base.clone()
            } else {
                format!(
                    "{}/{}",
                    base_url_str.trim_end_matches('/'),
                    rep_base_url.base
                )
            };
        }

        // Handle different types of segment information
        if let Some(segment_template) = representation.SegmentTemplate.as_ref().or_else(|| {
            // Try to get segment template from parent adaptation set
            mpd.periods
                .iter()
                .flat_map(|p| &p.adaptations)
                .find(|a| a.representations.iter().any(|r| r.id == representation.id))
                .and_then(|a| a.SegmentTemplate.as_ref())
        }) {
            if let Some(init_template) = &segment_template.initialization {
                let init_url = init_template.replace(
                    "$RepresentationID$",
                    &representation.id.clone().unwrap_or_default(),
                );

                // Resolve init segment URL against base URL
                let full_init_url = if init_url.starts_with("http") {
                    init_url
                } else {
                    if let Ok(mut parsed) = Url::parse(&base_url_str) {
                        parsed.path_segments_mut().unwrap();
                        parsed.join(&init_url).unwrap().to_string()
                    } else {
                        "broken".to_string()
                    }
                };

                init_segment = Some(full_init_url);
            }
            // Handle templated segments
            let duration = segment_template.duration.unwrap_or(1.0);
            let timescale = segment_template.timescale.unwrap_or(1);

            let segment_count = if let Some(timeline) = &segment_template.SegmentTimeline {
                timeline.segments.len()
            } else {
                // Estimate number of segments from MPD duration
                let period_duration = period.duration.unwrap_or(Duration::new(60, 0));
                ((period_duration.as_secs() * timescale as u64) as f64 / duration) as usize
            };

            // Limit to 10-20 segments for live streams
            let is_live = mpd.mpdtype.as_deref() == Some("dynamic");
            let segment_count = if is_live {
                20.min(segment_count)
            } else {
                segment_count
            };

            let times = if let Some(timeline) = &segment_template.SegmentTimeline {
                compute_segment_times(&timeline.segments)
            } else {
                // Fallback to number-based generation
                (0..segment_count)
                    .map(|i| i as i64 * duration as i64)
                    .collect()
            };

            for time in times {
                if let Some(media) = &segment_template.media {
                    let segment_url = media
                        .replace(
                            "$RepresentationID$",
                            &representation.id.clone().unwrap_or_default(),
                        )
                        .replace("$Time$", &time.to_string());

                    // Resolve segment URL against base URL
                    let full_url = if segment_url.starts_with("http") {
                        segment_url
                    } else {
                        if let Ok(mut parsed) = Url::parse(&base_url_str) {
                            parsed.path_segments_mut().unwrap();
                            parsed.join(&segment_url).unwrap().to_string()
                        } else {
                            "broken".to_string()
                        }
                    };

                    segments.push(full_url);
                }
            }
        } else if let Some(segment_list) = &representation.SegmentList {
            // Handle segment list
            for segment in &segment_list.segment_urls {
                if let Some(media) = &segment.media {
                    // Resolve segment URL against base URL
                    let full_url = if media.starts_with("http") {
                        media.clone()
                    } else {
                        format!("{}/{}", base_url_str.trim_end_matches('/'), media)
                    };

                    segments.push(full_url);
                }
            }
        } else if let Some(base_url_str) = &representation.BaseURL.get(0) {
            // Handle single segment representation
            segments.push(base_url_str.base.clone());
        } else {
            anyhow::bail!("Could not find segment information for representation");
        }

        // If it's a live stream, only keep the last few segments
        let is_live = mpd.mpdtype.as_deref() == Some("dynamic");
        if is_live && segments.len() > 20 {
            segments = segments
                .clone()
                .into_iter()
                .skip(segments.len() - 20)
                .collect();
        }

        Ok((segments, init_segment))
    }

    fn decrypt_segment(&self, data: &[u8]) -> anyhow::Result<Vec<u8>> {
        // Check if we have a key to decrypt with
        if self.stream_info.key.is_empty() {
            // No decryption needed, just write the data
            return Ok(data.to_vec());
        }

        let keys =
            std::collections::HashMap::from([("1".to_owned(), self.stream_info.key.clone())]);
        let mp4decrypt_result = mp4decrypt::mp4decrypt(data, keys, None);

        match &mp4decrypt_result {
            Ok(output) => {
                return Ok(output.to_vec());
            }
            Err(e) => {
                error!("Failed to run ffmpeg: {}", e);

                // If that fails, try using ffmpeg as fallback
                let mut child = Command::new("ffmpeg")
                    .arg("-y")
                    .arg("-decryption_key")
                    .arg(&self.stream_info.key)
                    .arg("-i")
                    .arg("pipe:0")
                    .arg("-c")
                    .arg("copy")
                    .arg("pipe:0")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()?;

                if let Some(mut input) = child.stdin.take() {
                    input.write_all(data)?;
                    input.flush()?;
                }

                if let Some(mut output) = child.stdout.take() {
                    let mut out: Vec<u8> = Vec::new();
                    output.read_to_end(&mut out)?;
                    return Ok(out);
                }
            }
        }

        Ok(data.to_vec())
    }

    fn download_and_process_segments(&mut self) -> anyhow::Result<()> {
        // Parse MPD and extract segments
        let ((video_segments, video_init), (audio_segments, audio_init)) = self.process_mpd()?;

        // Skip processing if we have no new segments
        if video_segments == self.last_processed_segments.0
            && audio_segments == self.last_processed_segments.1
        {
            return Ok(());
        }

        let client = reqwest::blocking::Client::builder()
            .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:133.0) Gecko/20100101 Firefox/133.0")
            .timeout(Duration::from_secs(30))
            .build()?;

        // Download init segments (only once)
        if let Some(video_init_url) = &video_init {
            self.stream_info.init_segments.remove("video");
            if let Ok(resp) = client.get(video_init_url).send() {
                if resp.status().is_success() {
                    if let Ok(bytes) = resp.bytes() {
                        self.stream_info
                            .init_segments
                            .insert("video".to_string(), bytes.to_vec());
                    }
                }
            }
        }

        if let Some(audio_init_url) = &audio_init {
            self.stream_info.init_segments.remove("audio");
            if let Ok(resp) = client.get(audio_init_url).send() {
                if resp.status().is_success() {
                    if let Ok(bytes) = resp.bytes() {
                        self.stream_info
                            .init_segments
                            .insert("audio".to_string(), bytes.to_vec());
                    }
                }
            }
        }

        let min_len = video_segments.len().min(audio_segments.len());

        for i in 0..min_len {
            if !self.is_active {
                break;
            }

            let video_url = &video_segments[i];
            let audio_url = &audio_segments[i];

            if self.last_processed_segments.0.contains(video_url)
                && self.last_processed_segments.1.contains(audio_url)
            {
                continue;
            }

            // Download and decrypt video
            let video_data = self.download_and_decrypt_segment(&client, video_url, "video")?;
            let video_file = self
                .temp_dir
                .join(format!("video_{}.mp4", self.sequence_number));
            fs::write(&video_file, &video_data)?;

            // Download and decrypt audio
            let audio_data = self.download_and_decrypt_segment(&client, audio_url, "audio")?;
            let audio_file = self
                .temp_dir
                .join(format!("audio_{}.mp4", self.sequence_number));
            fs::write(&audio_file, &audio_data)?;

            // Mux both streams with FFmpeg
            let ts_data = mux_to_ts(&video_file, &audio_file)?;
            self.pusher.write(&ts_data)?;

            fs::remove_file(&video_file).ok();
            fs::remove_file(&audio_file).ok();
        }

        self.last_processed_segments = (video_segments, audio_segments);
        Ok(())
    }

    fn download_and_decrypt_segment(
        &self,
        client: &reqwest::blocking::Client,
        url: &str,
        kind: &str,
    ) -> anyhow::Result<Vec<u8>> {
        let resp = client.get(url).send()?;
        if !resp.status().is_success() {
            anyhow::bail!("HTTP {} on {}", resp.status(), url);
        }

        let bytes = resp.bytes()?.to_vec();
        let combined = if let Some(init) = self.stream_info.init_segments.get(kind) {
            let mut full = init.clone();
            full.extend_from_slice(&bytes);
            full
        } else {
            bytes
        };

        let decrypted = self.decrypt_segment(&combined)?;
        Ok(decrypted)
    }

    pub fn stop(&mut self) -> anyhow::Result<()> {
        self.is_active = false;
        self.pusher.kill()?;
        Ok(())
    }

    pub fn run_streaming_loop(converter_arc: Arc<Mutex<Self>>) -> anyhow::Result<()> {
        {
            let mut converter = converter_arc.lock().unwrap();
            converter.start()?;
        }

        loop {
            {
                let mut converter = converter_arc.lock().unwrap();
                if !converter.is_active {
                    break;
                }
                if let Err(e) = converter.download_and_process_segments() {
                    error!(
                        "Error processing segments for {}: {}",
                        converter.stream_info.id, e
                    );
                    // Short pause to avoid rapid fail loops
                    thread::sleep(Duration::from_secs(1));
                }
            }

            // Sleep before fetching updates to MPD
            thread::sleep(Duration::from_secs(1));
        }

        Ok(())
    }
}

fn compute_segment_times(timeline: &[S]) -> Vec<i64> {
    let mut times = Vec::new();
    let mut current_time = timeline.first().and_then(|s| s.t).unwrap_or(0);

    for item in timeline {
        let repeat = item.r.unwrap_or(0);
        for _ in 0..=repeat {
            times.push(current_time);
            current_time += item.d;
        }
    }

    times
}

fn mux_to_ts(video_path: &Path, audio_path: &Path) -> anyhow::Result<Vec<u8>> {
    let output = Command::new("ffmpeg")
        .args(["-y", "-i"])
        .arg(video_path)
        .args(["-i"])
        .arg(audio_path)
        .args([
            "-map", "0:v:0", "-map", "1:a:0", "-c:v", "copy", "-c:a", "aac", "-f", "mpegts",
            "pipe:1",
        ])
        .output()?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("ffmpeg muxing failed: {}", err);
    }

    Ok(output.stdout)
}
