use std::env;
use std::iter::{Cycle, Peekable};
use std::time::{Duration, Instant};
use std::vec;

use async_trait::async_trait;
use crossbeam_channel::Sender;
use serde_derive::Deserialize;
use tokio::process::Command;

use crate::blocks::{Block, ConfigBlock, Update};
use crate::config::SharedConfig;
use crate::de::deserialize_update;
use crate::errors::*;
use crate::protocol::i3bar_event::I3BarEvent;
use crate::scheduler::Task;
use crate::signals::convert_to_valid_signal;
use crate::subprocess::spawn_child_async;
use crate::widgets::text::TextWidget;
use crate::widgets::{I3BarWidget, State};

pub struct Custom {
    id: usize,
    update_interval: Update,
    command: Option<String>,
    on_click: Option<String>,
    cycle: Option<Peekable<Cycle<vec::IntoIter<String>>>>,
    signal: Option<i32>,
    tx_update_request: Sender<Task>,
    pub json: bool,
    hide_when_empty: bool,
    shell: String,
    shared_config: SharedConfig,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields, default)]
pub struct CustomConfig {
    /// Update interval in seconds
    #[serde(deserialize_with = "deserialize_update")]
    pub interval: Update,

    /// Shell Command to execute & display
    pub command: Option<String>,

    /// Commands to execute and change when the button is clicked
    pub cycle: Option<Vec<String>>,

    /// Signal to update upon reception
    pub signal: Option<i32>,

    /// Parse command output if it contains valid bar JSON
    pub json: bool,

    pub hide_when_empty: bool,

    // TODO make a global config option
    pub shell: String,
}

impl Default for CustomConfig {
    fn default() -> Self {
        Self {
            interval: Update::Every(Duration::from_secs(10)),
            command: None,
            cycle: None,
            signal: None,
            json: false,
            hide_when_empty: false,
            shell: env::var("SHELL").unwrap_or_else(|_| "sh".to_owned()),
        }
    }
}

impl ConfigBlock for Custom {
    type Config = CustomConfig;

    fn new(
        id: usize,
        block_config: Self::Config,
        shared_config: SharedConfig,
        tx: Sender<Task>,
    ) -> Result<Self> {
        let mut custom = Custom {
            id,
            update_interval: block_config.interval,
            command: None,
            on_click: None,
            cycle: None,
            signal: None,
            tx_update_request: tx,
            json: block_config.json,
            hide_when_empty: block_config.hide_when_empty,
            shell: block_config.shell,
            shared_config,
        };

        if let Some(signal) = block_config.signal {
            // If the signal is not in the valid range we return an error
            custom.signal = Some(convert_to_valid_signal(signal)?);
        };

        if block_config.cycle.is_some() && block_config.command.is_some() {
            return Err(BlockError(
                "custom".to_string(),
                "`command` and `cycle` are mutually exclusive".to_string(),
            ));
        }

        if let Some(cycle) = block_config.cycle {
            custom.cycle = Some(cycle.into_iter().cycle().peekable());
            return Ok(custom);
        };

        if let Some(command) = block_config.command {
            custom.command = Some(command)
        };

        Ok(custom)
    }

    fn override_on_click(&mut self) -> Option<&mut Option<String>> {
        Some(&mut self.on_click)
    }
}

fn default_icon() -> String {
    String::from("")
}

fn default_state() -> State {
    State::Idle
}

#[derive(Deserialize)]
struct Output {
    #[serde(default = "default_icon")]
    icon: String,
    #[serde(default = "default_state")]
    state: State,
    text: String,
}

#[async_trait(?Send)]
impl Block for Custom {
    fn update_interval(&self) -> Update {
        self.update_interval.clone()
    }

    async fn render(&'_ mut self) -> Result<Vec<Box<dyn I3BarWidget>>> {
        let mut widget = TextWidget::new(self.id(), 0, self.shared_config.clone());

        let command_str = self
            .cycle
            .as_mut()
            .map(|c| c.peek().cloned().unwrap_or_else(|| "".to_owned()))
            .or_else(|| self.command.clone())
            .unwrap_or_else(|| "".to_owned());

        let raw_output = Command::new(&self.shell)
            .args(&["-c", &command_str])
            .output()
            .await
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_else(|e| e.to_string());

        let text = {
            if self.json {
                let output: Output = serde_json::from_str(&*raw_output).map_err(|e| {
                    BlockError("custom".to_string(), format!("Error parsing JSON: {}", e))
                })?;

                if !output.icon.is_empty() {
                    widget.set_icon(&output.icon)?;
                }

                widget.set_state(output.state);
                output.text
            } else {
                raw_output
            }
        };

        if text.is_empty() && self.hide_when_empty {
            Ok(Vec::new())
        } else {
            widget.set_text(text);
            Ok(vec![Box::new(widget)])
        }
    }

    async fn signal(&mut self, signal: i32) -> Result<()> {
        if let Some(sig) = self.signal {
            if sig == signal {
                self.tx_update_request.send(Task {
                    id: self.id,
                    update_time: Instant::now(),
                })?;
            }
        }
        Ok(())
    }

    fn click(&mut self, _e: I3BarEvent) -> Result<()> {
        let mut update = false;

        if let Some(ref on_click) = self.on_click {
            spawn_child_async(&self.shell, &["-c", on_click]).ok();
            update = true;
        }

        if let Some(ref mut cycle) = self.cycle {
            cycle.next();
            update = true;
        }

        if update {
            self.tx_update_request.send(Task {
                id: self.id,
                update_time: Instant::now(),
            })?;
        }

        Ok(())
    }

    fn id(&self) -> usize {
        self.id
    }
}
