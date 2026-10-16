//! Video streams on the display channel (`channel-display.c` stream
//! handling, `channel-display-mjpeg.c` and, for H.264, the scheduling in
//! `channel-display-gst.c`): frames are queued and shown at their
//! multimedia time, late ones are dropped, and stream reports tell the
//! server how playback is going so it can adapt the bitrate.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use bytes::Bytes;
use spice_canvas::Region;
use spice_proto::msg::display::{Clip, Rect, StreamReport};

/// After this many consecutive drops a report is sent early
/// (`STREAM_REPORT_DROP_SEQ_LEN_LIMIT`).
const DROP_SEQ_LEN_LIMIT: u32 = 3;

#[derive(Debug, Clone)]
pub enum FrameData {
    /// An MJPEG frame, decoded when it is shown.
    Jpeg(Bytes),
    /// A frame from a codec with inter-frame references (H.264), decoded
    /// on arrival because every frame must go through the decoder in order.
    #[cfg(feature = "h264")]
    Decoded(spice_decode::Image),
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub mm_time: u32,
    pub dest: Rect,
    pub data: FrameData,
}

/// A stream's decoder state.
pub enum Codec {
    Mjpeg,
    #[cfg(feature = "h264")]
    H264(Box<spice_decode::h264::H264Decoder>),
}

impl Codec {
    /// Whether late frames can be dropped before decoding. MJPEG frames
    /// stand alone; H.264 frames are references for later ones.
    fn independent_frames(&self) -> bool {
        matches!(self, Codec::Mjpeg)
    }
}

#[derive(Debug, Default)]
struct Report {
    id: u32,
    max_window: u32,
    timeout: Duration,
    start_time: Option<Instant>,
    start_frame_time: u32,
    num_frames: u32,
    num_drops: u32,
    drops_seq_len: u32,
}

pub struct Stream {
    pub codec: Codec,
    pub surface_id: u32,
    pub flags: u8,
    pub dest: Rect,
    pub clip: Option<Region>,
    queue: VecDeque<Frame>,
    /// The next frame to show, waiting for its time.
    scheduled: Option<Frame>,
    report: Option<Report>,
    pub frames_shown: u64,
    pub frames_dropped: u64,
    pub decode_errors: u64,
}

/// `spice_mmtime_diff`: wrapping difference of two mm times.
pub fn mm_diff(a: u32, b: u32) -> i32 {
    a.wrapping_sub(b) as i32
}

pub fn clip_region(clip: &Clip) -> Option<Region> {
    match clip {
        Clip::None => None,
        Clip::Rects(r) => Some(Region::from_rects(r)),
    }
}

impl Stream {
    pub fn new(codec: Codec, surface_id: u32, flags: u8, dest: Rect, clip: &Clip) -> Self {
        Self {
            codec,
            surface_id,
            flags,
            dest,
            clip: clip_region(clip),
            queue: VecDeque::new(),
            scheduled: None,
            report: None,
            frames_shown: 0,
            frames_dropped: 0,
            decode_errors: 0,
        }
    }

    /// `mjpeg_decoder_queue_frame` / `spice_gst_decoder_queue_frame`.
    /// `now` is the current mm time (`None` before the server sent one, in
    /// which case frames show immediately).
    pub fn queue_frame(&mut self, frame: Frame, now: Option<u32>) {
        if let Some(last) = self.queue.back()
            && mm_diff(frame.mm_time, last.mm_time) < 0
        {
            // "This should really not happen": restart the queue.
            self.queue.clear();
            self.scheduled = None;
        }
        if let Some(now) = now
            && mm_diff(frame.mm_time, now) < 0
            && self.codec.independent_frames()
        {
            // Late MJPEG frames don't affect later ones; drop early.
            self.frames_dropped += 1;
            return;
        }
        self.queue.push_back(frame);
        self.schedule(now);
    }

    /// `mjpeg_decoder_schedule`: pick the next frame that isn't late.
    /// For H.264 (`schedule_frame` in channel-display-gst.c) the newest
    /// frame is shown even when late, so the video doesn't freeze while
    /// the decoder can't keep up.
    pub fn schedule(&mut self, now: Option<u32>) {
        if self.scheduled.is_some() {
            return;
        }
        let show_last = !self.codec.independent_frames();
        while let Some(f) = self.queue.pop_front() {
            match now {
                Some(now)
                    if mm_diff(now, f.mm_time) > 0 && !(show_last && self.queue.is_empty()) =>
                {
                    self.frames_dropped += 1
                }
                _ => {
                    self.scheduled = Some(f);
                    return;
                }
            }
        }
    }

    /// Milliseconds until the scheduled frame is due (0 if due now).
    pub fn due_in(&self, now: Option<u32>) -> Option<u32> {
        let f = self.scheduled.as_ref()?;
        Some(match now {
            Some(now) => mm_diff(f.mm_time, now).max(0) as u32,
            None => 0,
        })
    }

    /// Take the scheduled frame if it is due, and schedule the next one.
    pub fn take_due(&mut self, now: Option<u32>) -> Option<Frame> {
        if self.due_in(now)? > 0 {
            return None;
        }
        let f = self.scheduled.take();
        self.frames_shown += 1;
        self.schedule(now);
        f
    }

    /// `display_handle_stream_activate_report`
    pub fn activate_report(&mut self, unique_id: u32, max_window: u32, timeout_ms: u32) {
        self.report = Some(Report {
            id: unique_id,
            max_window,
            timeout: Duration::from_millis(timeout_ms as u64),
            ..Default::default()
        });
    }

    /// `display_update_stream_report`: account for a frame that arrived
    /// with `margin` ms to spare (negative = late); returns a report to
    /// send when the window fills, times out, or drops pile up.
    pub fn update_report(
        &mut self,
        stream_id: u32,
        frame_time: u32,
        margin: i32,
        audio_delay: u32,
    ) -> Option<StreamReport> {
        let r = self.report.as_mut()?;
        let now = Instant::now();
        if r.num_frames == 0 {
            r.start_frame_time = frame_time;
            r.start_time = Some(now);
        }
        r.num_frames += 1;
        if margin < 0 {
            r.num_drops += 1;
            r.drops_seq_len += 1;
        } else {
            r.drops_seq_len = 0;
        }
        let elapsed = r.start_time.map_or(Duration::ZERO, |t| now - t);
        if r.num_frames >= r.max_window
            || elapsed >= r.timeout
            || r.drops_seq_len >= DROP_SEQ_LEN_LIMIT
        {
            let report = StreamReport {
                stream_id,
                unique_id: r.id,
                start_frame_mm_time: r.start_frame_time,
                end_frame_mm_time: frame_time,
                num_frames: r.num_frames,
                num_drops: r.num_drops,
                last_frame_delay: margin,
                audio_delay,
            };
            r.start_time = None;
            r.start_frame_time = 0;
            r.num_frames = 0;
            r.num_drops = 0;
            r.drops_seq_len = 0;
            return Some(report);
        }
        None
    }
}

/// `report_invalid_stream`: "UINT_MAX dropped frames out of zero" tells
/// the server a stream doesn't exist or can't be decoded.
pub fn invalid_stream_report(stream_id: u32) -> StreamReport {
    StreamReport {
        stream_id,
        unique_id: 0,
        start_frame_mm_time: 0,
        end_frame_mm_time: 0,
        num_frames: 0,
        num_drops: u32::MAX,
        last_frame_delay: 0,
        audio_delay: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(t: u32) -> Frame {
        Frame {
            mm_time: t,
            dest: Rect::new(0, 0, 1, 1),
            data: FrameData::Jpeg(Bytes::new()),
        }
    }

    fn stream() -> Stream {
        Stream::new(Codec::Mjpeg, 0, 0, Rect::new(0, 0, 1, 1), &Clip::None)
    }

    #[test]
    fn frames_wait_for_their_time_and_late_ones_drop() {
        let mut s = stream();
        s.queue_frame(frame(100), Some(50));
        s.queue_frame(frame(140), Some(50));
        s.queue_frame(frame(40), Some(50)); // older than the queue tail: restart, and late
        assert_eq!(s.frames_dropped, 1);
        assert_eq!(s.due_in(Some(50)), None, "queue was reset");
        s.queue_frame(frame(100), Some(50));
        s.queue_frame(frame(140), Some(50));
        assert_eq!(s.due_in(Some(50)), Some(50));
        assert!(s.take_due(Some(60)).is_none());
        assert_eq!(s.take_due(Some(100)).unwrap().mm_time, 100);
        // At 150 the 140 frame is already late when scheduled after the
        // first: it was scheduled while not late, so it's shown.
        assert_eq!(s.take_due(Some(150)).unwrap().mm_time, 140);
        assert_eq!(s.frames_shown, 2);
        // Frames behind the clock when the previous one finishes are dropped.
        s.queue_frame(frame(200), Some(190));
        s.queue_frame(frame(210), Some(190));
        s.take_due(Some(215));
        assert_eq!(s.due_in(Some(215)), None);
        assert_eq!(s.frames_dropped, 2);
    }

    #[cfg(feature = "h264")]
    #[test]
    fn h264_keeps_late_frames_until_newer_ones_arrive() {
        let dec = spice_decode::h264::H264Decoder::new().unwrap();
        let mut s = Stream::new(
            Codec::H264(Box::new(dec)),
            0,
            0,
            Rect::new(0, 0, 1, 1),
            &Clip::None,
        );
        // Already late on arrival, but it's all there is: shown.
        s.queue_frame(frame(40), Some(50));
        assert_eq!(s.frames_dropped, 0);
        assert_eq!(s.take_due(Some(50)).unwrap().mm_time, 40);
        // Two late frames: only the newest is shown.
        s.queue_frame(frame(60), Some(100));
        s.queue_frame(frame(70), Some(100));
        assert_eq!(
            s.take_due(Some(100)).unwrap().mm_time,
            60,
            "scheduled first"
        );
        s.queue_frame(frame(80), Some(100));
        s.queue_frame(frame(90), Some(100));
        assert_eq!(s.take_due(Some(100)).unwrap().mm_time, 70);
        assert_eq!(s.take_due(Some(100)).unwrap().mm_time, 90);
        assert_eq!(s.frames_dropped, 1);
    }

    #[test]
    fn reports() {
        let mut s = stream();
        assert!(s.update_report(3, 10, 5, 0).is_none(), "inactive");
        s.activate_report(77, 3, 10_000);
        assert!(s.update_report(3, 10, 5, 0).is_none());
        assert!(s.update_report(3, 20, -1, 0).is_none());
        let r = s.update_report(3, 30, 4, u32::MAX).unwrap();
        assert_eq!(
            (
                r.unique_id,
                r.num_frames,
                r.num_drops,
                r.start_frame_mm_time,
                r.end_frame_mm_time
            ),
            (77, 3, 1, 10, 30)
        );
        // Three drops in a row report immediately.
        s.activate_report(78, 100, 10_000);
        s.update_report(3, 1, -1, 0);
        s.update_report(3, 2, -1, 0);
        assert_eq!(s.update_report(3, 3, -1, 0).unwrap().num_drops, 3);
        assert_eq!(invalid_stream_report(5).num_drops, u32::MAX);
        assert_eq!(mm_diff(1, u32::MAX), 2);
    }
}
