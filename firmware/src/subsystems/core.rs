use embassy_sync::channel::Channel;
use esp_idf_svc::hal::task::embassy_sync::EspRawMutex;

use crate::subsystems::buttons::ButtonChange;

pub struct Core {
    buttons_channel: &'static Channel<EspRawMutex, ButtonChange, 32>,
}

impl Core {
    pub fn new(buttons_channel: &'static Channel<EspRawMutex, ButtonChange, 32>) -> Self {
        Self { buttons_channel }
    }

    pub async fn on_button_press(&self) {
        while let Ok(button_change) = self.buttons_channel.try_receive() {
            match button_change {
                ButtonChange::Pressed { button_id } => {
                    log::info!("Button {:?} pressed", button_id);
                }
                ButtonChange::Released { button_id } => {
                    log::info!("Button {:?} released", button_id);
                }
            }
        }
    }
}
