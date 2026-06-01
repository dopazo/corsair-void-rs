use std::time::{Duration, Instant};

use log::{debug, info};

use super::{AudioController, AudioError};

/// Wall-clock cap for PulseAudio connect/operation waits. Bounds any freeze if
/// the daemon is wedged (these calls run on the single tray event loop).
const PA_TIMEOUT: Duration = Duration::from_secs(3);

pub struct LinuxAudioController {
    device_index: Option<u32>,
    /// Channel count of the matched Corsair source, captured in `find_device`.
    channels: Option<u8>,
}

impl LinuxAudioController {
    pub fn new() -> Self {
        Self {
            device_index: None,
            channels: None,
        }
    }
}

impl AudioController for LinuxAudioController {
    fn find_device(&mut self) -> Result<bool, AudioError> {
        let (mut mainloop, context) = Self::connect_pulse()?;

        let introspect = context.introspect();
        let found = std::sync::Arc::new(std::sync::Mutex::new(None));
        let found_clone = found.clone();

        let op = introspect.get_source_info_list(move |result| {
            if let libpulse_binding::callbacks::ListResult::Item(source) = result {
                let name = source
                    .description
                    .as_ref()
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                debug!("PulseAudio source: {} (index={})", name, source.index);
                if name.to_lowercase().contains("corsair") {
                    let channels = source.sample_spec.channels;
                    info!(
                        "Found Corsair source: {} (index={}, channels={})",
                        name, source.index, channels
                    );
                    *found_clone.lock().unwrap() = Some((source.index, channels));
                }
            }
        });

        Self::wait_for_op(&mut mainloop, &op)?;

        let found = *found.lock().unwrap();
        self.device_index = found.map(|(idx, _)| idx);
        self.channels = found.map(|(_, ch)| ch);
        Ok(self.device_index.is_some())
    }

    fn set_boost_db(&self, db: u8) -> Result<(), AudioError> {
        let index = self.device_index.ok_or(AudioError::DeviceNotFound)?;
        let (mut mainloop, context) = Self::connect_pulse()?;
        let mut introspect = context.introspect();

        // Convert dB boost to PulseAudio volume.
        // 0 dB = Volume::NORMAL (100%), +5 dB ≈ 178%, +10 dB ≈ 316%
        let normal = libpulse_binding::volume::Volume::NORMAL.0 as f64;
        let factor = 10.0_f64.powf(db as f64 / 20.0);
        let pa_vol = (normal * factor) as u32;
        let volume = libpulse_binding::volume::Volume(pa_vol);
        // The first arg to pa_cvolume_set is the channel COUNT, not an index.
        // Hardcoding 2 makes PA reject the cvolume on the mono Corsair source
        // (PA_ERR_INVALID). Use the source's real channel count, falling back to
        // 1, which the server always accepts and remaps.
        let channels = self.channels.unwrap_or(1).max(1);
        let channel_volumes = libpulse_binding::volume::ChannelVolumes::default()
            .set(channels as u32, volume)
            .clone();

        info!(
            "Setting PulseAudio boost: +{} dB (PA volume: {}, factor: {:.2}, channels: {})",
            db, pa_vol, factor, channels
        );
        let op = introspect.set_source_volume_by_index(index, &channel_volumes, None);
        Self::wait_for_op(&mut mainloop, &op)?;
        Ok(())
    }

    fn get_boost_db(&self) -> Result<u8, AudioError> {
        let index = self.device_index.ok_or(AudioError::DeviceNotFound)?;
        let (mut mainloop, context) = Self::connect_pulse()?;
        let introspect = context.introspect();
        let boost = std::sync::Arc::new(std::sync::Mutex::new(0u8));
        let boost_clone = boost.clone();

        let op = introspect.get_source_info_by_index(index, move |result| {
            if let libpulse_binding::callbacks::ListResult::Item(source) = result {
                let avg = source.volume.avg().0 as f64;
                let normal = libpulse_binding::volume::Volume::NORMAL.0 as f64;
                let ratio = avg / normal;
                let db = if ratio > 1.0 {
                    (20.0 * ratio.log10()).round() as u8
                } else {
                    0
                };
                *boost_clone.lock().unwrap() = db;
            }
        });

        Self::wait_for_op(&mut mainloop, &op)?;
        let result = *boost.lock().unwrap();
        Ok(result)
    }

    fn boost_available(&self) -> bool {
        // PulseAudio natively supports volume above 100%, no extra software needed
        true
    }
}

impl LinuxAudioController {
    fn connect_pulse() -> Result<
        (
            libpulse_binding::mainloop::standard::Mainloop,
            libpulse_binding::context::Context,
        ),
        AudioError,
    > {
        use libpulse_binding::context::Context;
        use libpulse_binding::mainloop::standard::Mainloop;

        let mut mainloop =
            Mainloop::new().ok_or(AudioError::ApiError("Failed to create mainloop".into()))?;
        let mut context = Context::new(&mainloop, "corsair-void")
            .ok_or(AudioError::ApiError("Failed to create context".into()))?;

        context
            .connect(None, libpulse_binding::context::FlagSet::NOFLAGS, None)
            .map_err(|e| AudioError::ApiError(format!("connect: {:?}", e)))?;

        // Bounded, non-blocking handshake: iterate(false) never blocks, so a
        // wedged daemon can't freeze the tray — we bail after PA_TIMEOUT.
        let deadline = Instant::now() + PA_TIMEOUT;
        loop {
            match mainloop.iterate(false) {
                libpulse_binding::mainloop::standard::IterateResult::Quit(_)
                | libpulse_binding::mainloop::standard::IterateResult::Err(_) => {
                    return Err(AudioError::ApiError("mainloop error".into()));
                }
                libpulse_binding::mainloop::standard::IterateResult::Success(_) => {}
            }
            match context.get_state() {
                libpulse_binding::context::State::Ready => break,
                libpulse_binding::context::State::Failed
                | libpulse_binding::context::State::Terminated => {
                    return Err(AudioError::ApiError("context failed".into()));
                }
                _ => {}
            }
            if Instant::now() >= deadline {
                return Err(AudioError::ApiError("PulseAudio connect timed out".into()));
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        Ok((mainloop, context))
    }

    /// Drive the mainloop until `op` finishes. Returns an error if PA rejects or
    /// cancels it (so a rejected volume change is no longer silently reported as
    /// success), or if it doesn't complete within PA_TIMEOUT. On error the caller
    /// drops the op/context/mainloop, which tears down the abandoned operation.
    fn wait_for_op<F: ?Sized>(
        mainloop: &mut libpulse_binding::mainloop::standard::Mainloop,
        op: &libpulse_binding::operation::Operation<F>,
    ) -> Result<(), AudioError> {
        let deadline = Instant::now() + PA_TIMEOUT;
        loop {
            match mainloop.iterate(false) {
                libpulse_binding::mainloop::standard::IterateResult::Quit(_)
                | libpulse_binding::mainloop::standard::IterateResult::Err(_) => {
                    return Err(AudioError::ApiError("mainloop error".into()));
                }
                libpulse_binding::mainloop::standard::IterateResult::Success(_) => {}
            }
            match op.get_state() {
                libpulse_binding::operation::State::Done => return Ok(()),
                libpulse_binding::operation::State::Cancelled => {
                    return Err(AudioError::ApiError(
                        "PulseAudio operation cancelled".into(),
                    ));
                }
                libpulse_binding::operation::State::Running => {}
            }
            if Instant::now() >= deadline {
                return Err(AudioError::ApiError(
                    "PulseAudio operation timed out".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
