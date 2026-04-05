use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel, mutex::Mutex};
use esp_idf_svc::hal::{
    gpio::{Input, PinDriver},
    i2c::I2cDriver,
    task::embassy_sync::EspRawMutex,
};

#[derive(Copy, Clone, Debug)]
pub enum ButtonId {
    Switch1,
    Switch2,
    Switch3,
    Switch4,
    Switch5,
    ThreePositionSwitchUp,
    ThreePositionSwitchDown,
}

#[derive(Copy, Clone, Debug)]
pub enum ButtonChange {
    Pressed { button_id: ButtonId },
    Released { button_id: ButtonId },
}

pub struct CompleteButtonState {
    switch1: bool,
    switch2: bool,
    switch3: bool,
    switch4: bool,
    switch5: bool,
    three_position_switch: i8, // -1 for down, 0 for middle, 1 for up
}

#[derive(Copy, Clone)]
enum ButtonIrq {
    Edge,
}

static BUTTON_IRQ_CH: Channel<CriticalSectionRawMutex, ButtonIrq, 8> = Channel::new();

pub struct ButtonSubsystem {
    state: CompleteButtonState,
    buttons_channel: &'static Channel<EspRawMutex, ButtonChange, 32>,
    shared_i2c_bus: &'static Mutex<EspRawMutex, I2cDriver<'static>>,
    pub interrupt_pin_1: PinDriver<'static, Input>,
    pub interrupt_pin_2: PinDriver<'static, Input>,
}

impl ButtonSubsystem {
    const EXPANDER_I2C_ADDRESS: u8 = 0x20;

    pub async fn new(
        buttons_channel: &'static Channel<EspRawMutex, ButtonChange, 32>,
        shared_i2c_bus: &'static Mutex<EspRawMutex, I2cDriver<'static>>,
        mut interrupt_pin_1: PinDriver<'static, Input>,
        mut interrupt_pin_2: PinDriver<'static, Input>,
    ) -> Self {
        // Configure the interrupt pins to trigger on any edge
        interrupt_pin_1
            .enable_interrupt()
            .expect("Failed to enable GPIO interrupt 1");
        interrupt_pin_2
            .enable_interrupt()
            .expect("Failed to enable GPIO interrupt 2");

        // Initialize GPIO expander over I2C
        let mut i2c_bus = shared_i2c_bus.lock().await;
        let _ = i2c_bus.write(Self::EXPANDER_I2C_ADDRESS, &[0x00, 0xFF], 10); // Set all pins as inputs (IODIRA)
        let _ = i2c_bus.write(Self::EXPANDER_I2C_ADDRESS, &[0x01, 0xFF], 10); // Set all pins as inputs (IODIRB)
        drop(i2c_bus);

        // Return the initialized subsystem
        Self {
            state: CompleteButtonState {
                switch1: false,
                switch2: false,
                switch3: false,
                switch4: false,
                switch5: false,
                three_position_switch: 0,
            },
            buttons_channel,
            shared_i2c_bus,
            interrupt_pin_1,
            interrupt_pin_2,
        }
    }

    pub fn on_gpio_interrupt_1() {
        let _ = BUTTON_IRQ_CH.try_send(ButtonIrq::Edge);
    }

    pub fn on_gpio_interrupt_2() {
        let _ = BUTTON_IRQ_CH.try_send(ButtonIrq::Edge);
    }

    async fn read_gpio_expander_raw(&mut self) -> (u8, u8) {
        // Lock the I2C bus to read the state of the buttons from the I2C expander
        let mut i2c_bus = self.shared_i2c_bus.lock().await;

        let gpio_a_register = 0x12;
        let gpio_b_register = 0x13;

        // Read GPIO A and GPIO B registers from the I2C expander
        let mut gpio_a_buffer = [0];
        let _ = i2c_bus.write_read(
            Self::EXPANDER_I2C_ADDRESS,
            &[gpio_a_register],
            &mut gpio_a_buffer,
            10,
        );

        let mut gpio_b_buffer = [0];
        let _ = i2c_bus.write_read(
            Self::EXPANDER_I2C_ADDRESS,
            &[gpio_b_register],
            &mut gpio_b_buffer,
            10,
        );

        // Doesn't hurt to explicitly drop the lock guard here
        drop(i2c_bus);

        (gpio_a_buffer[0], gpio_b_buffer[0])
    }

    pub async fn interrupt_handler(&mut self) {
        loop {
            let _ = BUTTON_IRQ_CH.receive().await;

            let (gpio_a, gpio_b) = self.read_gpio_expander_raw().await;

            // Reset interrupt ASAP to minimize the time spent handling the interrupt
            self.interrupt_pin_1
                .enable_interrupt()
                .expect("Failed to re-enable GPIO interrupt 1");
            self.interrupt_pin_2
                .enable_interrupt()
                .expect("Failed to re-enable GPIO interrupt 2");

            // Calculate button changes and send them to the core subsystem
            let new_state = Self::calculate_new_state(gpio_a, gpio_b);
            let button_changes = Self::calculate_button_changes(&self.state, &new_state);

            for change in button_changes.iter().flatten() {
                let _ = self
                    .buttons_channel
                    .try_send(*change)
                    .expect("Failed to send button change to core subsystem");
            }

            self.state = new_state;
        }
    }

    fn calculate_new_state(gpio_a: u8, gpio_b: u8) -> CompleteButtonState {
        // Mapping:
        // - SW1 -> GPB0
        // - SW2 -> GPB1
        // - SW3 -> GPB2
        // - SW4 -> GPA2
        // - SW5 -> GPA3
        // - SW6 (3-position switch up) -> GPA0
        // - SW6 (3-position switch down) -> GPA1
        CompleteButtonState {
            switch1: (gpio_b & 0b0000_0001) == 0,
            switch2: (gpio_b & 0b0000_0010) == 0,
            switch3: (gpio_b & 0b0000_0100) == 0,
            switch4: (gpio_a & 0b0000_0100) == 0,
            switch5: (gpio_a & 0b0000_1000) == 0,
            three_position_switch: if (gpio_a & 0b0000_0001) == 0 {
                1
            } else if (gpio_a & 0b0000_0010) == 0 {
                -1
            } else {
                0
            },
        }
    }

    fn calculate_button_changes(
        old_state: &CompleteButtonState,
        new_state: &CompleteButtonState,
    ) -> [Option<ButtonChange>; 6] {
        // Unholy mess - but it works
        [
            if old_state.switch1 != new_state.switch1 {
                Some(if new_state.switch1 {
                    ButtonChange::Pressed {
                        button_id: ButtonId::Switch1,
                    }
                } else {
                    ButtonChange::Released {
                        button_id: ButtonId::Switch1,
                    }
                })
            } else {
                None
            },
            if old_state.switch2 != new_state.switch2 {
                Some(if new_state.switch2 {
                    ButtonChange::Pressed {
                        button_id: ButtonId::Switch2,
                    }
                } else {
                    ButtonChange::Released {
                        button_id: ButtonId::Switch2,
                    }
                })
            } else {
                None
            },
            if old_state.switch3 != new_state.switch3 {
                Some(if new_state.switch3 {
                    ButtonChange::Pressed {
                        button_id: ButtonId::Switch3,
                    }
                } else {
                    ButtonChange::Released {
                        button_id: ButtonId::Switch3,
                    }
                })
            } else {
                None
            },
            if old_state.switch4 != new_state.switch4 {
                Some(if new_state.switch4 {
                    ButtonChange::Pressed {
                        button_id: ButtonId::Switch4,
                    }
                } else {
                    ButtonChange::Released {
                        button_id: ButtonId::Switch4,
                    }
                })
            } else {
                None
            },
            if old_state.switch5 != new_state.switch5 {
                Some(if new_state.switch5 {
                    ButtonChange::Pressed {
                        button_id: ButtonId::Switch5,
                    }
                } else {
                    ButtonChange::Released {
                        button_id: ButtonId::Switch5,
                    }
                })
            } else {
                None
            },
            if old_state.three_position_switch != new_state.three_position_switch {
                Some(match new_state.three_position_switch {
                    1 => ButtonChange::Pressed {
                        button_id: ButtonId::ThreePositionSwitchUp,
                    },
                    -1 => ButtonChange::Pressed {
                        button_id: ButtonId::ThreePositionSwitchDown,
                    },
                    0 => match old_state.three_position_switch {
                        1 => ButtonChange::Released {
                            button_id: ButtonId::ThreePositionSwitchUp,
                        },
                        -1 => ButtonChange::Released {
                            button_id: ButtonId::ThreePositionSwitchDown,
                        },
                        _ => return [None; 6], // Invalid state
                    },
                    _ => return [None; 6], // Invalid state
                })
            } else {
                None
            },
        ]
    }
}
