#![no_std]
#![no_main]

mod fmt;
mod rtclock;

use core::f32::consts;
use core::fmt::Write;

use arrayvec::ArrayString;
#[cfg(not(feature = "defmt"))]
use panic_halt as _;
use crate::fmt::unwrap;
use business_logic::{door::DoorEvent, logger::{Logger, LoggerEvent, TemperatureSample}};
use business_logic::timestamp::Timestamp;

#[cfg(feature = "defmt")]
use {defmt_rtt as _, panic_probe as _};

use embassy_executor::Spawner;
use embassy_stm32::{bind_interrupts, exti::ExtiInput, peripherals};
use embassy_stm32::{gpio::{Level, Output, Pull, Speed}, i2c::{ErrorInterruptHandler, EventInterruptHandler, I2c}, rtc::{Rtc, RtcConfig}, time::Hertz, Config};
use embassy_sync::blocking_mutex::raw::ThreadModeRawMutex;
use embassy_sync::channel::{Channel, Sender};
use embassy_time::{Duration, Ticker, Timer};
use fmt::{info, warn};
use rtclock::{Rtclock};

const AMBIENT_ADDRESS: u8 = 0x45; // I2C address for ambient temperature sensor.
const VACCINE_ADDRESS: u8 = 0x44; // I2C address for vaccine temperature sensor.
const SENSOR_REGISTER: u8 = 0x00; // Register to read temperature data.
const SENSOR_CONVERSION_TIME: Duration = Duration::from_millis(51); // Time to wait for sensor conversion.

// Communicate events between tasks using a channel.
static CHANNEL: Channel<ThreadModeRawMutex, LoggerEvent, 8> = Channel::new();

// enum ButtonEvent {
//     Pressed,
//     Released,
// }

// enum Events {
//     Button(ButtonEvent),
//     TempReading((f32, f32)), // (ambient temperature, vaccine temperature)
// }

struct DualTempSensor<I2C> {
    i2c: I2C,
    amb_address: u8,
    vax_address: u8,
    enable_bar: Output<'static>,
}

impl<I2C> DualTempSensor<I2C> {
    pub fn new(i2c: I2C, amb_address: u8, vax_address: u8, enable_bar: Output<'static>) -> Self {
        Self { i2c, amb_address, vax_address, enable_bar }
    }
}

impl<I2C> DualTempSensor<I2C>
where
    I2C: embedded_hal_async::i2c::I2c,
{
    pub async fn read_temperature_celsius(&mut self) -> TemperatureSample {
        self.enable_bar.set_low(); // Enable the temperature sensor.
        Timer::after(SENSOR_CONVERSION_TIME).await; // Wait for sensor to stabilize.
        let mut buf = [0u8; 2];
        let amb_temp  = match self.i2c.write_read(self.amb_address, &[SENSOR_REGISTER], &mut buf).await {
            Ok(_) => Some(f32::from(i16::from_be_bytes(buf)) * 0.0078125), // Convert to Celsius
            Err(_) => {
                warn!("Failed to read from temperature sensor");
                None
            }
        };

        let vax_temp = match self.i2c.write_read(self.vax_address, &[SENSOR_REGISTER], &mut buf).await {
            Ok(_) => Some(f32::from(i16::from_be_bytes(buf)) * 0.0078125), // Convert to Celsius
            Err(_) => {
                warn!("Failed to read from temperature sensor");
                None
            }
        };

        self.enable_bar.set_high(); // Disable the temperature sensor.

        TemperatureSample {
            ambient: amb_temp,
            vaccine: vax_temp,
        }
    }
}


#[embassy_executor::main]
async fn main(spawner: Spawner) {

    // Chip peripheral configuration
    let mut config = Config::default();
    {
        use embassy_stm32::rcc::*;
        use embassy_stm32::rcc::mux::{Adcsel, Clk48sel, I2c1sel};

        // Adjust the configuration from the default.
        // Default for Config.rcc is hse=None, hsi=false, SAI1,2=None
        config.rcc.msi = Some(MSIRange::RANGE4M); // Multi-speed Osc. = 4 MHz

        // PLL creates 48 MHz at its output (PLLCLK).
        config.rcc.pll = Some(Pll {
            source: PllSource::MSI,
            prediv: PllPreDiv::DIV1,
            mul: PllMul::MUL24,
            divp: None, // This was DIV7 in the CubeMX config, but the output would only be for serial audio, which we are not using.
            divq: Some(PllQDiv::DIV2),
            divr: Some(PllRDiv::DIV2), // for sysclk of 48 MHz
        });

        // Clock busses
        config.rcc.sys = Sysclk::PLL1_R; // 48 MHz
        config.rcc.ahb_pre = AHBPrescaler::DIV1; // HCLK = 48 MHz
        config.rcc.apb1_pre = APBPrescaler::DIV1;
        config.rcc.apb2_pre = APBPrescaler::DIV1;

        // Low-speed oscillators
        config.rcc.ls = LsConfig {
            rtc: RtcClockSource::LSE,
            lsi: false, // Not using LSI for either watchdog or RTC.
            lse: Some(LseConfig { frequency: Hertz(32768), mode: LseMode::Oscillator(LseDrive::Low) }),
        };

        // Reconfigure some of the clock mux struct fields.
        config.rcc.mux.adcsel = Adcsel::SYS;  // C firmware used SAI1R clock, also 48 MHz.  Not sure why.
        config.rcc.mux.clk48sel = Clk48sel::PLLSAI1_Q; // TODO: code doc says this is the PLL48M1CLK, but datasheet says PLL48M1CLK comes from PLL1.  C code uses the SAI1clk.  
        config.rcc.mux.i2c1sel = I2c1sel::PCLK1;
    }
    let p = embassy_stm32::init(config);

    // GPIOs
    let mut pwrv_nen = Output::new(p.PA15, Level::High, Speed::Low); // Power enable for the temperature sensor.
    pwrv_nen.set_low(); // Enable the temperature sensor.
    let mut led = Output::new(p.PB0, Level::High, Speed::Low);
    let mut btn = ExtiInput::new(p.PB5, p.EXTI5, Pull::Up);

    // RTC initialization
    let mut rtc = Rtc::new(p.RTC, RtcConfig::default());
    rtc.set_daylight_savings(false);
    let rt_clock = if Rtclock::is_running(&rtc) {
        info!("RTC is running, using existing RTCW value...");
        Rtclock::from_running(rtc)
    } else {
        // RTC was not running, so we need to initialize it.
        info!("RTC not running, initializing...");
        let rtcw = 0_u32; // TODO: Get the RTCW value from non-volatile storage or set to 0.
        Rtclock::from_rtcw(rtc, rtcw)
    };


    // I2C and temp sensor initialization.
    bind_interrupts!(struct Irqs {
        I2C1_EV => EventInterruptHandler<peripherals::I2C1>;
        I2C1_ER => ErrorInterruptHandler<peripherals::I2C1>;
    });

    let mut i2c = I2c::new(
        p.I2C1, 
        p.PB6, 
        p.PB7, 
        Irqs,
        p.DMA1_CH6,
        p.DMA1_CH7, 
        Hertz(400_000),
        Default::default(),
    );
    let mut temp_sensor = DualTempSensor::new(i2c, AMBIENT_ADDRESS, VACCINE_ADDRESS, pwrv_nen);

    // Spawn the button task
    spawner.spawn(button(btn, CHANNEL.sender())).unwrap();
    spawner.spawn(led_blink(led)).unwrap();
    spawner.spawn(get_temperature(temp_sensor, CHANNEL.sender())).unwrap();

    warn!("Starting main loop");

    loop {
        let event = CHANNEL.receive().await;
        let now = rt_clock.get_timestamp();

        match event {
            LoggerEvent::DoorEvent(DoorEvent::Opened) => {
                info!("Button pressed event received");
                // let then = rtc.now().unwrap();
                // info!("time: {:?}:{:?}", then.minute(), then.second());
            }
            LoggerEvent::DoorEvent(DoorEvent::Closed) => {
                info!("Button released event received");
            }
            LoggerEvent::TemperatureSample(temperature) => {
                // let ts = rt_clock.get_timestamp();
                info!("Time: {}, TAMB: {} °C, TVC: {} °C", now.seconds, temperature.ambient, temperature.vaccine);
                // let ts = rt_clock.get_timestamp();
                info!("{=str}", now.create_iso8601_str());
            }
        }

    }
}

#[embassy_executor::task]
async fn button(mut btn: ExtiInput<'static>, msg: Sender<'static, ThreadModeRawMutex, LoggerEvent, 8>) {
    loop {
        btn.wait_for_falling_edge().await;
        info!("Button pressed/door open!");
        msg.send(LoggerEvent::DoorEvent(DoorEvent::Opened)).await;
        // Debounce delay
        Timer::after(Duration::from_millis(50)).await;
        // Wait for release (rising edge)
        btn.wait_for_rising_edge().await;
        info!("Button released/door closed!");
        msg.send(LoggerEvent::DoorEvent(DoorEvent::Closed)).await;
        // Debounce delay
        Timer::after(Duration::from_millis(50)).await;
    }
}

#[embassy_executor::task]
async fn led_blink(mut led: Output<'static>) {
    loop {
        led.set_high();
        Timer::after(Duration::from_millis(500)).await;
        led.set_low();
        Timer::after(Duration::from_millis(500)).await;
    }
}

#[embassy_executor::task]
async fn get_temperature(
    mut temp_sensor: DualTempSensor<I2c<'static, embassy_stm32::mode::Async>>,
    msg: Sender<'static, ThreadModeRawMutex, LoggerEvent, 8>,
) {
    let mut ticker = Ticker::every(Duration::from_secs(10)); // Read every 10 seconds
    loop {
        let temperatures = temp_sensor.read_temperature_celsius().await;
        msg.send(LoggerEvent::TemperatureSample(temperatures)).await;
        ticker.next().await;
    }
}
