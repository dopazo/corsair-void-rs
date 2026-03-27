use log::{debug, info};

use super::{AudioController, AudioError};

pub struct LinuxAudioController {
    device_index: Option<u32>,
}

impl LinuxAudioController {
    pub fn new() -> Self {
        Self { device_index: None }
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
                    info!("Found Corsair source: {} (index={})", name, source.index);
                    *found_clone.lock().unwrap() = Some(source.index);
                }
            }
        });

        Self::wait_for_op(&mut mainloop, &op);

        self.device_index = *found.lock().unwrap();
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
        let channel_volumes =
            libpulse_binding::volume::ChannelVolumes::default().set(2, volume).clone();

        info!("Setting PulseAudio boost: +{} dB (PA volume: {}, factor: {:.2})", db, pa_vol, factor);
        let op = introspect.set_source_volume_by_index(index, &channel_volumes, None);
        Self::wait_for_op(&mut mainloop, &op);
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

        Self::wait_for_op(&mut mainloop, &op);
        Ok(*boost.lock().unwrap())
    }

    fn boost_available(&self) -> bool {
        // PulseAudio natively supports volume above 100%, no extra software needed
        true
    }
}

impl LinuxAudioController {
    fn connect_pulse(
    ) -> Result<
        (
            libpulse_binding::mainloop::standard::Mainloop,
            libpulse_binding::context::Context,
        ),
        AudioError,
    > {
        use libpulse_binding::context::Context;
        use libpulse_binding::mainloop::standard::Mainloop;

        let mut mainloop = Mainloop::new()
            .ok_or(AudioError::ApiError("Failed to create mainloop".into()))?;
        let mut context = Context::new(&mainloop, "corsair-void")
            .ok_or(AudioError::ApiError("Failed to create context".into()))?;

        context
            .connect(None, libpulse_binding::context::FlagSet::NOFLAGS, None)
            .map_err(|e| AudioError::ApiError(format!("connect: {:?}", e)))?;

        loop {
            match mainloop.iterate(true) {
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
        }

        Ok((mainloop, context))
    }

    fn wait_for_op<F: ?Sized>(
        mainloop: &mut libpulse_binding::mainloop::standard::Mainloop,
        op: &libpulse_binding::operation::Operation<F>,
    ) {
        loop {
            match mainloop.iterate(true) {
                libpulse_binding::mainloop::standard::IterateResult::Quit(_)
                | libpulse_binding::mainloop::standard::IterateResult::Err(_) => break,
                libpulse_binding::mainloop::standard::IterateResult::Success(_) => {}
            }
            if op.get_state() != libpulse_binding::operation::State::Running {
                break;
            }
        }
    }
}
