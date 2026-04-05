mod services;
mod subsystems;

use crate::subsystems::buttons::{ButtonChange, ButtonSubsystem};
use embassy_futures::join::join;
use embassy_sync::{channel::Channel, mutex::Mutex};
use esp_idf_svc::hal::{
    gpio::{PinDriver, Pull},
    i2c::{self, I2cDriver},
    peripherals::Peripherals,
    task::{block_on, embassy_sync::EspRawMutex},
    units::Hertz,
};
use static_cell::StaticCell;
use subsystems::core;

// Statics
static I2C_BUS: StaticCell<Mutex<EspRawMutex, I2cDriver<'static>>> = StaticCell::new();
static BUTTON_CHANNEL: StaticCell<Channel<EspRawMutex, ButtonChange, 32>> = StaticCell::new();

fn main() {
    // It is necessary to call this function once. Otherwise, some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    // Global peripherals
    let peripherals = Peripherals::take().expect("Failed to take peripherals");

    // 1.1 Create button subsystem
    let button_channel = Channel::<EspRawMutex, ButtonChange, 32>::new();
    let button_channel: &'static Channel<EspRawMutex, ButtonChange, 32> =
        BUTTON_CHANNEL.init(button_channel);

    let gpio_interrupt_pin_1 = PinDriver::input(peripherals.pins.gpio38, Pull::Up)
        .expect("Failed to configure GPIO interrupt pin 1");
    let gpio_interrupt_pin_2 = PinDriver::input(peripherals.pins.gpio39, Pull::Up)
        .expect("Failed to configure GPIO interrupt pin 2");
    let i2c_bus = I2cDriver::new(
        peripherals.i2c0,
        peripherals.pins.gpio8,
        peripherals.pins.gpio9,
        &i2c::config::Config::default().baudrate(Hertz(100_000)),
    )
    .expect("Failed to initialize I2C bus");
    let shared_i2c_bus = Mutex::<EspRawMutex, I2cDriver<'static>>::new(i2c_bus);
    let shared_i2c_bus: &'static Mutex<EspRawMutex, I2cDriver<'static>> =
        I2C_BUS.init(shared_i2c_bus);

    let button_subsystem_future = ButtonSubsystem::new(
        button_channel,
        shared_i2c_bus,
        gpio_interrupt_pin_1,
        gpio_interrupt_pin_2,
    );

    // 2.1 Initialize core subsystem
    let core = core::Core::new(button_channel);

    block_on(async {
        let mut button_subsystem = button_subsystem_future.await;

        log::info!("Si4 booted!");

        join(button_subsystem.interrupt_handler(), core.on_button_press()).await;

        log::error!("Task join returned unexpectedly");
    })

    // 1.2 Initialize button subsystem tasks
}
