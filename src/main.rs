#![no_std]
#![no_main]

mod fmt;

#[cfg(not(feature = "defmt"))]
use panic_halt as _;
#[cfg(feature = "defmt")]
use {defmt_rtt as _, panic_probe as _};

use embassy_executor::Spawner;
use embassy_stm32::{gpio::{Level, Output, Speed}, rtc::{DateTime, Rtc, RtcConfig}, time::Hertz, Config};
use embassy_time::{Duration, Timer};
use fmt::{info, warn};

#[embassy_executor::main]
async fn main(_spawner: Spawner) {

    // Chip peripheral configuration
    let mut config = Config::default();
    {
        use embassy_stm32::rcc::*;
        use embassy_stm32::rcc::mux::{Adcsel, Clk48sel, I2c1sel};
use embassy_stm32::gpio::{Input, Pull, AnyPin, ExtiInput};
use embassy_stm32::exti::ExtiInput as EmbassyExtiInput;

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
    let mut led = Output::new(p.PB0, Level::High, Speed::Low);

    // RTC, from embassy example, but not using the chrono crate.
    let now: DateTime = DateTime::from(
        2025, 6, 19, embassy_stm32::rtc::DayOfWeek::Thursday, 9, 42, 0).expect("invalid date");

    let mut rtc = Rtc::new(p.RTC, RtcConfig::default());
    rtc.set_daylight_savings(false);
    rtc.set_datetime(now).expect("datetime not set");

    let mut counter: usize = 0;

    loop {
        warn!("Hello, World!");
        led.set_high();
        Timer::after(Duration::from_millis(500)).await;
        led.set_low();
        Timer::after(Duration::from_millis(500)).await;

        counter += 1;
        if counter % 10 == 0 {
            let then = rtc.now().unwrap();
            info!("time: {:?}:{:?}", then.minute(), then.second());
        }
    }
}
