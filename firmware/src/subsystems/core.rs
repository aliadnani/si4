use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};

pub struct Core {
    button_channel: Channel<CriticalSectionRawMutex, u32, 32>,
}

impl Core {
    pub async fn on_button_press_task(&self) {
        let receiver = self.button_channel.receiver();

        loop {
            log::info!("Button pressed!");
        }

        
    }
}
