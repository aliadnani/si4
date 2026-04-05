use embassy_futures::select::{select3, Either3};
use embassy_sync::{channel::Channel, mutex::Mutex};
use embassy_time::{Duration, Timer};
use esp_idf_svc::hal::{
    gpio::{Input, InterruptType, PinDriver},
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

#[derive(Copy, Clone)]
pub struct CompleteButtonState {
    switch1: bool,
    switch2: bool,
    switch3: bool,
    switch4: bool,
    switch5: bool,
    three_position_switch: i8, // -1 for down, 0 for middle, 1 for up
}

pub struct ButtonSubsystem {
    state: CompleteButtonState,
    buttons_channel: &'static Channel<EspRawMutex, ButtonChange, 32>,
    shared_i2c_bus: &'static Mutex<EspRawMutex, I2cDriver<'static>>,
    pub interrupt_pin_1: PinDriver<'static, Input>,
    pub interrupt_pin_2: PinDriver<'static, Input>,
}

impl ButtonSubsystem {
    const EXPANDER_I2C_ADDRESS: u8 = 0x20;
    const I2C_TIMEOUT_TICKS: u32 = 100;
    const POLL_INTERVAL_MS: u64 = 20;
    const DEBOUNCE_MS: u64 = 12;
    // For configuring the all as inputs
    const REG_IODIRA: u8 = 0x00;
    const REG_IODIRB: u8 = 0x01;
    // For reading the state of the pins
    const REG_GPIOA: u8 = 0x12;
    const REG_GPIOB: u8 = 0x13;

    fn write_reg(i2c_bus: &mut I2cDriver<'static>, reg: u8, value: u8) -> bool {
        match i2c_bus.write(
            Self::EXPANDER_I2C_ADDRESS,
            &[reg, value],
            Self::I2C_TIMEOUT_TICKS,
        ) {
            Ok(()) => true,
            Err(e) => {
                log::error!(
                    "Expander write failed: reg=0x{:02X}, value=0x{:02X}, err={:?}",
                    reg,
                    value,
                    e
                );
                false
            }
        }
    }

    fn read_reg(i2c_bus: &mut I2cDriver<'static>, reg: u8) -> Option<u8> {
        let mut buffer = [0];
        match i2c_bus.write_read(
            Self::EXPANDER_I2C_ADDRESS,
            &[reg],
            &mut buffer,
            Self::I2C_TIMEOUT_TICKS,
        ) {
            Ok(()) => Some(buffer[0]),
            Err(e) => {
                log::error!("Expander read failed: reg=0x{:02X}, err={:?}", reg, e);
                None
            }
        }
    }

    pub async fn new(
        buttons_channel: &'static Channel<EspRawMutex, ButtonChange, 32>,
        shared_i2c_bus: &'static Mutex<EspRawMutex, I2cDriver<'static>>,
        mut interrupt_pin_1: PinDriver<'static, Input>,
        mut interrupt_pin_2: PinDriver<'static, Input>,
    ) -> Self {
        // Set the interrupt mode once; wait_for_any_edge() will re-arm internally.
        interrupt_pin_1
            .set_interrupt_type(InterruptType::AnyEdge)
            .expect("Failed to set GPIO interrupt type 1");
        interrupt_pin_2
            .set_interrupt_type(InterruptType::AnyEdge)
            .expect("Failed to set GPIO interrupt type 2");

        // Initialize GPIO expander over I2C
        let mut i2c_bus = shared_i2c_bus.lock().await;
        let configured = [
            Self::write_reg(&mut i2c_bus, Self::REG_IODIRA, 0xFF),
            Self::write_reg(&mut i2c_bus, Self::REG_IODIRB, 0xFF),
        ]
        .iter()
        .all(|ok| *ok);

        if configured {
            log::info!("Button expander base setup configured");
        } else {
            log::error!("Button expander base setup failed; continuing for diagnostics");
        }

        let gpio_a = Self::read_reg(&mut i2c_bus, Self::REG_GPIOA).unwrap_or(0xFF);
        let gpio_b = Self::read_reg(&mut i2c_bus, Self::REG_GPIOB).unwrap_or(0xFF);

        let initial_state = Self::calculate_new_state(gpio_a, gpio_b);
        log::info!(
            "Button subsystem initialized with expander state: GPIOA=0b{:08b}, GPIOB=0b{:08b}",
            gpio_a,
            gpio_b
        );

        // Return the initialized subsystem
        Self {
            state: initial_state,
            buttons_channel,
            shared_i2c_bus,
            interrupt_pin_1,
            interrupt_pin_2,
        }
    }

    async fn read_gpio_expander_raw(&mut self) -> (u8, u8) {
        // Lock the I2C bus to read the state of the buttons from the I2C expander
        let mut i2c_bus = self.shared_i2c_bus.lock().await;
        let gpio_a = Self::read_reg(&mut i2c_bus, Self::REG_GPIOA).unwrap_or(0xFF);
        let gpio_b = Self::read_reg(&mut i2c_bus, Self::REG_GPIOB).unwrap_or(0xFF);
        (gpio_a, gpio_b)
    }

    async fn read_gpio_expander_debounced(&mut self) -> (u8, u8) {
        let first = self.read_gpio_expander_raw().await;
        Timer::after(Duration::from_millis(Self::DEBOUNCE_MS)).await;
        let second = self.read_gpio_expander_raw().await;

        if first == second {
            return second;
        }

        Timer::after(Duration::from_millis(Self::DEBOUNCE_MS)).await;
        self.read_gpio_expander_raw().await
    }

    pub async fn interrupt_handler(&mut self) {
        loop {
            let _ = select3(
                self.interrupt_pin_1.wait_for_any_edge(),
                self.interrupt_pin_2.wait_for_any_edge(),
                Timer::after(Duration::from_millis(Self::POLL_INTERVAL_MS)),
            )
            .await;

            let (gpio_a, gpio_b) = self.read_gpio_expander_debounced().await;

            // Calculate button changes and send them to the core subsystem
            let new_state = Self::calculate_new_state(gpio_a, gpio_b);
            let button_changes = Self::calculate_button_changes(&self.state, &new_state);

            for change in button_changes.iter().flatten() {
                self.buttons_channel.send(*change).await;
            }

            self.state = new_state;
        }
    }

    fn calculate_new_state(gpio_a: u8, gpio_b: u8) -> CompleteButtonState {
        // Mapping:
        // - SW1 -> GPA0
        // - SW2 -> GPA1
        // - SW3 -> GPA2
        // - SW4 -> GPB2
        // - SW5 -> GPB3
        // - SW6 (3-position switch up) -> GPB0
        // - SW6 (3-position switch down) -> GPB1
        CompleteButtonState {
            switch1: (gpio_a & 0b0000_0001) == 0,
            switch2: (gpio_a & 0b0000_0010) == 0,
            switch3: (gpio_a & 0b0000_0100) == 0,
            switch4: (gpio_b & 0b0000_0100) == 0,
            switch5: (gpio_b & 0b0000_1000) == 0,
            three_position_switch: if (gpio_b & 0b0000_0001) == 0 {
                -1
            } else if (gpio_b & 0b0000_0010) == 0 {
                1
            } else {
                0
            },
        }
    }

    fn change_for_switch(old: bool, new: bool, button_id: ButtonId) -> Option<ButtonChange> {
        if old == new {
            return None;
        }

        Some(if new {
            ButtonChange::Pressed { button_id }
        } else {
            ButtonChange::Released { button_id }
        })
    }

    fn change_for_three_position(old: i8, new: i8) -> Option<ButtonChange> {
        if old == new {
            return None;
        }

        match new {
            1 => Some(ButtonChange::Pressed {
                button_id: ButtonId::ThreePositionSwitchUp,
            }),
            -1 => Some(ButtonChange::Pressed {
                button_id: ButtonId::ThreePositionSwitchDown,
            }),
            0 => match old {
                1 => Some(ButtonChange::Released {
                    button_id: ButtonId::ThreePositionSwitchUp,
                }),
                -1 => Some(ButtonChange::Released {
                    button_id: ButtonId::ThreePositionSwitchDown,
                }),
                _ => None,
            },
            _ => None,
        }
    }

    fn calculate_button_changes(
        old_state: &CompleteButtonState,
        new_state: &CompleteButtonState,
    ) -> [Option<ButtonChange>; 6] {
        [
            Self::change_for_switch(old_state.switch1, new_state.switch1, ButtonId::Switch1),
            Self::change_for_switch(old_state.switch2, new_state.switch2, ButtonId::Switch2),
            Self::change_for_switch(old_state.switch3, new_state.switch3, ButtonId::Switch3),
            Self::change_for_switch(old_state.switch4, new_state.switch4, ButtonId::Switch4),
            Self::change_for_switch(old_state.switch5, new_state.switch5, ButtonId::Switch5),
            Self::change_for_three_position(
                old_state.three_position_switch,
                new_state.three_position_switch,
            ),
        ]
    }
}
