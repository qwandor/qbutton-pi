// Copyright 2018 Google LLC
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

mod assistant;
mod config;
mod radio;

use crate::{
    assistant::{get_token, make_request},
    config::Config,
};
use eyre::Report;
use log::{debug, error, info, trace, warn};
use radio::Radio;
use rfbutton::{Code, decode};
use std::time::{Duration, Instant};

/// The minimum amount of time between repeating a command for the same button code.
const MIN_REPEAT_DURATION: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> Result<(), Report> {
    pretty_env_logger::init();

    let config = Config::from_file()?;

    let mut radio = Radio::init()?;
    let mut last_code_time = Instant::now();
    let mut last_code = Code {
        value: 0,
        length: 0,
    };

    loop {
        match radio.receive() {
            Ok(pulses) => {
                if pulses.len() > 10 {
                    debug!("{} pulses: {:?}...", pulses.len(), &pulses[0..10]);
                } else {
                    debug!("{} pulses: {:?}", pulses.len(), pulses);
                }
                match decode(&pulses) {
                    Ok(code) => {
                        if code.length > 0 {
                            info!("Decoded: {:?}", code);
                            let now = Instant::now();
                            if code != last_code || now > last_code_time + MIN_REPEAT_DURATION {
                                handle_code(&config, &code).await?;
                                last_code = code;
                                last_code_time = now;
                            } else {
                                trace!("Ignoring repeated code.");
                            }
                        } else {
                            warn!("Decoded 0 bits.");
                        }
                    }
                    Err(e) => {
                        error!("Decode error: {}", e);
                    }
                }
            }
            Err(e) => {
                error!("Receive error: {}", e);
            }
        }
    }
}

async fn handle_code(config: &Config, code: &Code) -> Result<(), Report> {
    if let Some(command) = config.commands.get(code) {
        info!("Code {:?} corresponds to command {:?}.", code, command);
        let token = get_token(config).await?;
        make_request(config, &token, command).await?;
    } else {
        info!("No command for code {:?}.", code);
    }
    Ok(())
}
