//! SDL2 audio output.
//!
//! Opens an audio device at 32768 Hz stereo i16. A background callback
//! pulls samples from a shared `Arc<Mutex<VecDeque<i16>>>`. The main
//! emulator loop calls `pump` once per frame to refill the queue from
//! the core's `Nds::drain_audio`.
//!
//! Underrun → callback fills with silence.

use sdl2::audio::{AudioCallback, AudioDevice, AudioSpecDesired};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

const SAMPLE_RATE: i32 = 32768;
/// Callback buffer length, in stereo *frames* (each frame = 2 i16 samples).
const CALLBACK_FRAMES: u16 = 512;
/// How many stereo i16 samples we let queue up before discarding. 1 second
/// of audio at 32768 Hz = 65 536 samples × 2 channels.
const MAX_QUEUE: usize = 65_536 * 2;

type SharedQueue = Arc<Mutex<VecDeque<i16>>>;

struct Callback {
    queue: SharedQueue,
}

impl AudioCallback for Callback {
    type Channel = i16;

    fn callback(&mut self, out: &mut [i16]) {
        let mut q = self.queue.lock().unwrap();
        for sample in out.iter_mut() {
            *sample = q.pop_front().unwrap_or(0);
        }
    }
}

pub struct AudioOutput {
    pub queue: SharedQueue,
    _device: AudioDevice<Callback>,
}

impl AudioOutput {
    pub fn new(sdl: &sdl2::Sdl) -> Option<Self> {
        let audio = match sdl.audio() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("warning: SDL2 audio init failed: {} — silent run", e);
                return None;
            }
        };
        let spec = AudioSpecDesired {
            freq: Some(SAMPLE_RATE),
            channels: Some(2),
            samples: Some(CALLBACK_FRAMES),
        };
        let queue: SharedQueue = Arc::new(Mutex::new(VecDeque::with_capacity(8192)));
        let queue_for_cb = Arc::clone(&queue);
        let device = match audio.open_playback(None, &spec, |_| Callback { queue: queue_for_cb }) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("warning: open_playback failed: {} — silent run", e);
                return None;
            }
        };
        device.resume();
        Some(AudioOutput { queue, _device: device })
    }

    /// Push interleaved stereo samples into the shared queue. Drops old
    /// samples if the queue grows past `MAX_QUEUE` to keep latency bounded
    /// when the emulator outpaces realtime.
    pub fn push(&self, samples: &[i16]) {
        let mut q = self.queue.lock().unwrap();
        if q.len() + samples.len() > MAX_QUEUE {
            let drop = q.len() + samples.len() - MAX_QUEUE;
            for _ in 0..drop { q.pop_front(); }
        }
        q.extend(samples.iter().copied());
    }
}
