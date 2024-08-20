// Copyright 2024 Google LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::time::{Duration, Instant};

use cc1101::{
    lowlevel::types::AutoCalibration, Cc1101, FilterLength, Modulation, RadioMode, SyncMode,
    TargetAmplitude,
};
use embedded_hal_bus::spi::ExclusiveDevice;
use eyre::{bail, eyre, Report, WrapErr};
use log::{debug, info, trace};
use rppal::{
    gpio::{Event, Gpio, InputPin, Level, OutputPin, Trigger},
    hal::Delay,
    spi::{Bus, Mode, SlaveSelect, Spi},
};

/// The GPIO pin to which the 433 MHz receiver's data pin is connected.
const RX_PIN: u8 = 27;
/// The GPIO pin to which the 433 MHz receiver's chip-select pin is connected.
const CS_PIN: u8 = 25;

const MAX_PULSE_LENGTH: Duration = Duration::from_millis(10);
const BREAK_PULSE_LENGTH: Duration = Duration::from_millis(7);

pub struct Radio {
    _cc1101: Cc1101<ExclusiveDevice<Spi, OutputPin, Delay>>,
    rx_pin: InputPin,
}

impl Radio {
    pub fn init() -> Result<Self, Report> {
        // Create the device.
        let gpio = Gpio::new()?;
        let mut rx_pin = gpio.get(RX_PIN)?.into_input();
        let cs = gpio.get(CS_PIN)?.into_output();
        let spibus = Spi::new(Bus::Spi0, SlaveSelect::Ss0, 1_000_000, Mode::Mode0)?;
        let spi = ExclusiveDevice::new(spibus, cs, Delay)?;
        let mut cc1101 =
            Cc1101::new(spi).map_err(|e| eyre!("Error creating CC1101 device: {:?}", e))?;

        // Reset it, in case it is in some unexpected state.
        cc1101
            .reset()
            .map_err(|e| eyre!("Error resetting CC1101 device: {:?}", e))?;

        // Log some hardware info.
        let (partnum, version) = cc1101
            .get_hw_info()
            .map_err(|e| eyre!("Error getting hardware info: {:?}", e))?;
        info!("CC1101 part number {}, version {}", partnum, version);

        // Configure it to receive our button signals.
        cc1101
            .set_frequency(433940000)
            .map_err(|e| eyre!("Error setting frequency: {:?}", e))?;
        cc1101.set_raw_mode().map_err(|e| eyre!("{:?}", e))?;
        // Frequency synthesizer IF 211 kHz. Doesn't seem to affect big button, but affects sensitivity to small remote.
        cc1101
            .set_synthesizer_if(152_300)
            .map_err(|e| eyre!("{:?}", e))?;
        // DC blocking filter enabled, OOK modulation, manchester encoding disabled, no preamble/sync.
        cc1101
            .set_sync_mode(SyncMode::Disabled)
            .map_err(|e| eyre!("{:?}", e))?;
        cc1101
            .set_modulation(Modulation::OnOffKeying)
            .map_err(|e| eyre!("{:?}", e))?;
        // Channel bandwidth and data rate.
        cc1101.set_chanbw(232_000).map_err(|e| eyre!("{:?}", e))?;
        cc1101.set_data_rate(3_000).map_err(|e| eyre!("{:?}", e))?;
        // Automatically calibrate when going from IDLE to RX or TX.
        // XOSC stable timeout was being set to 64, but this doesn't seem important.
        cc1101
            .set_autocalibration(AutoCalibration::FromIdle)
            .map_err(|e| eyre!("{:?}", e))?;
        // Medium hysteresis, 16 channel filter samples, normal operation, OOK decision boundary 12 dB. Seems to affect sensitivity to small remote.
        cc1101
            .set_agc_filter_length(FilterLength::Samples32)
            .map_err(|e| eyre!("{:?}", e))?;
        // All gain settings can be used, maximum possible LNA gain, 36 dB target value.
        cc1101
            .set_agc_target(TargetAmplitude::Db42)
            .map_err(|e| eyre!("{:?}", e))?;
        cc1101
            .set_radio_mode(RadioMode::Receive)
            .map_err(|e| eyre!("{:?}", e))?;

        // Enable interrupts.
        rx_pin.set_interrupt(Trigger::Both, None)?;

        Ok(Self {
            _cc1101: cc1101,
            rx_pin,
        })
    }

    pub fn receive(&mut self) -> Result<Vec<u16>, Report> {
        debug!("Waiting for interrupt...");
        let Some(event) = self.rx_pin.poll_interrupt(false, None)? else {
            bail!("No initial event.");
        };
        debug!("Initial event: {:?}", event);
        let mut last_timestamp = Instant::now();
        let mut pulses = Vec::new();

        debug!("Waiting for initial break pulse...");
        // Wait for a long pulse to start.
        let mut last_pulse = Duration::default();
        while let Some(event) = self.rx_pin.poll_interrupt(false, None)? {
            let timestamp = Instant::now();
            let pulse_length = timestamp - last_timestamp;
            last_timestamp = timestamp;

            if event.trigger == Trigger::RisingEdge && pulse_length > BREAK_PULSE_LENGTH {
                trace!(
                    "Found possible initial break pulse {} μs.",
                    pulse_length.as_micros()
                );
            } else if event.trigger == Trigger::FallingEdge
                && last_pulse > BREAK_PULSE_LENGTH
                && pulse_length < BREAK_PULSE_LENGTH
            {
                debug!(
                    "Found initial break pulse {} μs and first pulse {} μs.",
                    last_pulse.as_micros(),
                    pulse_length.as_micros()
                );
                pulses.push(
                    last_pulse
                        .as_micros()
                        .try_into()
                        .wrap_err("Pulse length too long")?,
                );
                pulses.push(
                    pulse_length
                        .as_micros()
                        .try_into()
                        .wrap_err("Pulse length too long")?,
                );
                break;
            } else {
                trace!(
                    "Ignoring {} μs {:?} pulse.",
                    pulse_length.as_micros(),
                    previous_level(&event),
                );
            }

            last_pulse = pulse_length;
        }

        debug!("Reading pulses...");
        while let Some(event) = self.rx_pin.poll_interrupt(false, Some(MAX_PULSE_LENGTH))? {
            let timestamp = Instant::now();
            let pulse_length = timestamp - last_timestamp;
            pulses.push(
                pulse_length
                    .as_micros()
                    .try_into()
                    .wrap_err("Pulse length too long")?,
            );
            if pulse_length > BREAK_PULSE_LENGTH {
                debug!(
                    "Found final {:?} break pulse {} μs.",
                    previous_level(&event),
                    pulse_length.as_micros(),
                );
                break;
            }
            last_timestamp = timestamp;
        }

        Ok(pulses)
    }
}

/// Given an interreupt, returns the level of the pin before the interrupt trigger.
fn previous_level(event: &Event) -> Level {
    if event.trigger == Trigger::RisingEdge {
        Level::Low
    } else {
        Level::High
    }
}
