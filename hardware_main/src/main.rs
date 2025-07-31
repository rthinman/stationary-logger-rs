#![no_std]
#![no_main]

mod fmt;

use core::f32::consts;
use core::fmt::Write;

use arrayvec::ArrayString;
#[cfg(not(feature = "defmt"))]
use panic_halt as _;
use crate::fmt::unwrap;
use business_logic::timestamp::Timestamp;

#[cfg(feature = "defmt")]
use {defmt_rtt as _, panic_probe as _};

use embassy_executor::Spawner;
use embassy_stm32::{bind_interrupts, exti::ExtiInput, peripherals};
use embassy_stm32::{gpio::{Level, Output, Pull, Speed}, i2c::{ErrorInterruptHandler, EventInterruptHandler, I2c}, rtc::{DateTime, Rtc, RtcConfig}, time::Hertz, Config};
use embassy_sync::blocking_mutex::raw::ThreadModeRawMutex;
use embassy_sync::channel::{Channel, Sender};
use embassy_time::{Duration, Ticker, Timer};
use fmt::{info, warn};

const AMBIENT_ADDRESS: u8 = 0x45; // I2C address for ambient temperature sensor.
const VACCINE_ADDRESS: u8 = 0x44; // I2C address for vaccine temperature sensor.
const SENSOR_REGISTER: u8 = 0x00; // Register to read temperature data.
const SENSOR_CONVERSION_TIME: Duration = Duration::from_millis(51); // Time to wait for sensor conversion.

// Communicate events between tasks using a channel.
static CHANNEL: Channel<ThreadModeRawMutex, Events, 8> = Channel::new();

enum ButtonEvent {
    Pressed,
    Released,
}

enum Events {
    Button(ButtonEvent),
    TempReading((f32, f32)), // (ambient temperature, vaccine temperature)
}

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
    pub async fn read_temperature_celsius(&mut self) -> Result<(f32, f32), &str> {
        self.enable_bar.set_low(); // Enable the temperature sensor.
        Timer::after(SENSOR_CONVERSION_TIME).await; // Wait for sensor to stabilize.
        let mut buf = [0u8; 2];
        self.i2c.write_read(self.amb_address, &[SENSOR_REGISTER], &mut buf).await.or(Err("Failed to read from temperature sensor"))?;
        let amb_temp = i16::from_be_bytes(buf);
        self.i2c.write_read(self.vax_address, &[SENSOR_REGISTER], &mut buf).await.or(Err("Failed to read from temperature sensor"))?;
        let vax_temp = i16::from_be_bytes(buf);
        self.enable_bar.set_high(); // Disable the temperature sensor.
        Ok((f32::from(amb_temp) * 0.0078125, f32::from(vax_temp) * 0.0078125)) // Convert to Celsius
    }
}

const RTC_BACKUP_KEY_INDEX: usize = 0; // Index to RTC backup register where key is stored
const RTC_BACKUP_RTCW_INDEX: usize = 1; // Index to RTC backup register where RTCW is stored
const RTC_BACKUP_KEY_VALUE: u32 = 0xA53C4B69; // Value stored at RTC_BACKUP_KEY_INDEX if RTCW value is good
const EMBASSY_DATETIME_OFFSET: u16 = 2000; // Offset for the year in DateTime, since embassy-stm32 uses 2000-2099, but the RTC uses 0-99.

struct Rtclock {
    rtc: Rtc, // <'static, embassy_stm32::rtc::RtcConfig>
    rtcw: u32,
}

impl Rtclock {
    /// Create a new Rtclock instance if the RTC is already running.
    pub fn from_running(mut rtc: Rtc) -> Self {
        let rtcw = rtc.read_backup_register(RTC_BACKUP_RTCW_INDEX)
            .unwrap_or(0); // Read the RTCW value from the backup register, or 0 if not set.
        Self { rtc, rtcw }
    }
    /// Create a new Rtclock instance with a specific RTCW value.
    /// This is typically used when the RTC is starting from a power outage.
    pub fn from_rtcw(mut rtc: Rtc, rtcw: u32) -> Self {
        // Store the RTCW value in the backup register.
        rtc.write_backup_register(RTC_BACKUP_RTCW_INDEX, rtcw);
        // Write the key to the backup register to indicate that the RTCW value is valid.
        rtc.write_backup_register(RTC_BACKUP_KEY_INDEX, RTC_BACKUP_KEY_VALUE);
        // Convert to datetime and set RTC.
        let dt = Rtclock::seconds_to_datetime(rtcw);
        rtc.set_datetime(dt).expect("Failed to set datetime");
        // Return the Rtclock instance.
        Self { rtc, rtcw }
    }

    /// Get RELT, the uptime in seconds since first boot.
    pub fn get_uptime_seconds(&self) -> u32 {
        // Get the current time in seconds since the epoch.
        let now = self.rtc.now().unwrap();
        // Convert to seconds since the epoch (0, 3, 1).
        Rtclock::datetime_to_seconds(now).expect("Failed to convert datetime to seconds")
    }

    /// Get RTCWake, the value of RELT at the last "brownout" event.
    pub fn get_rtcw(&self) -> u32 {
        self.rtcw
    }

    // Static methods for Rtclock

    /// Check if the RTC is running.
    pub fn is_running(rtc: &Rtc) -> bool {
        // Check if the RTC is running by reading the backup register.
        rtc.read_backup_register(RTC_BACKUP_KEY_INDEX).unwrap_or(0) == RTC_BACKUP_KEY_VALUE
    }

    /// Convert seconds since the epoch (0, 3, 1) to a DateTime.
    pub fn seconds_to_datetime(seconds: u32) -> DateTime {
        // Get the total number of days represented, plus the time of day.
        // A u32 can hold up to 49,710 days, which is about 136 years.
        let (days, hour, minute, second) = Timestamp::seconds_to_dhms(seconds);

        // Calculate the Julian date from the number of days, since computational epoch
        // March 1, 0000.
        // Based on the algorithm from below, but not yet using the Euclidean 
        // affine function optimizations:
        // Neri C, Schneider L. "Euclidean affine functions and their application 
        // to calendar algorithms". Softw Pract Exper. 2022;1-34. doi: 10.1002/spe.3172.
        // https://onlinelibrary.wiley.com/doi/full/10.1002/spe.3172
        // References to variable names are to the paper above, Section 5, which uses uppercase.
        let n_1 = 4 * days + 3;  // N1
        let year_computational = n_1 / 1461;  // Y
        let n_y = n_1 % 1461 / 4; // N_Y
        let n_2 = 5 * n_y + 461;  // N_2
        let m = n_2 / 153; // M
        let day_julian: u8 = (n_2 % 153 / 5 + 1) as u8; // D_J (skipped computing D)
        // J = 1{M>=13}
        let j = if m >= 13 {
            1
        } else {
            0
        };

        let month_julian: u8 = (m - 12 * j) as u8; // M_J
        // Embassy's DateTime uses a year offset of 2000, so we need to add that.
        let year_julian: u16 = (year_computational + j) as u16 + EMBASSY_DATETIME_OFFSET; // Y_J + 2000

        // We do not use day of week, so the choice is arbitrary.
        DateTime::from(year_julian, month_julian, day_julian, embassy_stm32::rtc::DayOfWeek::Monday, hour as u8, minute as u8, second as u8).expect("Invalid date")
    }

    /// Convert a DateTime to seconds since the epoch (0, 3, 1) Julian date.
    pub fn datetime_to_seconds(datetime: DateTime) -> Option<u32> {
        // The STM32 RTC only holds year values 0-99.
        // Embassy's DateTime assumes the year is 2000-2099, so get last two digits.

        let year: u32 = (datetime.year() % 100).into(); 
        let month: u32 = datetime.month().into();
        if year == 0 && month < 3 {
            // The computational calendar does not support this combination with
            // unsigned values, and we should always initialize the RTC to later than this.
            return None;
        }
        let day: u32 = datetime.day().into();
        let hour: u32 = datetime.hour().into();
        let minute: u32 = datetime.minute().into();
        let second: u32 = datetime.second().into();

        let j: u32 = if month <= 2 {
            // This is because the computational calendar starts on March 1st.
            1
        } else {
            0
        };
        let y = year - j;
        let m = month + 12 * j; // Month is 1-12, but we need to adjust for the computational calendar.
        let d = day - 1; // Day is 1-31.

        let y0 = 1461 * y / 4;
        let m0 = (153 * m - 457) / 5;

        let days_since_epoch =  y0 + m0 + d;

        // Calculate the total seconds since the epoch.
        Some(days_since_epoch * 86400 + hour * 3600 + minute * 60 + second)
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

    // Temporary test code to check the conversion from seconds to DateTime.
    let dt = Rtclock::seconds_to_datetime(0);
    info!("should be march 1 2000: {}-{:02}-{:02} {:02}:{:02}:{:02}", 
        dt.year(), dt.month(), dt.day(), dt.hour(), dt.minute(), dt.second());
    let dt = Rtclock::seconds_to_datetime(86400);
    info!("should be march 2 2000: {}-{:02}-{:02} {:02}:{:02}:{:02}", 
        dt.year(), dt.month(), dt.day(), dt.hour(), dt.minute(), dt.second());

    // Test converting ISO 8601 duration to seconds and back.
    let duration_str = "P1DT2H3M4S"; // 1 day, 2 hours, 3 minutes, 4 seconds
    let (days, hours, minutes, seconds) = Timestamp::parse_duration(duration_str).expect("Failed to parse duration");
    info!("Parsed duration: {} days, {} hours, {} minutes, {} seconds", days, hours, minutes, seconds);
    let total_seconds = days * 86400 + hours * 3600 + minutes * 60 + seconds;
    assert_eq!(total_seconds, 93784, "Total seconds should be 93784");
    info!("Total seconds: {}", total_seconds);
    let iso_duration = Timestamp::seconds_to_iso8601_duration(total_seconds);
    info!("ISO 8601 Duration: {}", iso_duration.as_str());

    let reconverted_seconds = Rtclock::datetime_to_seconds(dt).expect("Failed to convert DateTime to seconds");
    info!("Reconverted seconds: {}", reconverted_seconds);

    let dt2: DateTime = DateTime::from(2000, 8, 15, embassy_stm32::rtc::DayOfWeek::Monday, 0, 0, 0)
        .expect("Failed to create DateTime");
    let seconds = Rtclock::datetime_to_seconds(dt2).expect("Failed to convert DateTime to seconds");
    // Should be 14,428,800.
    info!("DateTime 2000-08-15 00:00:00 converted to seconds: {}", seconds);
    
        // RTC initialization
    let mut rtc = Rtc::new(p.RTC, RtcConfig::default());
    rtc.set_daylight_savings(false);
    let timestamp = if Rtclock::is_running(&rtc) {
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
        match CHANNEL.receive().await {
            Events::Button(ButtonEvent::Pressed) => {
                info!("Button pressed event received");
                // let then = rtc.now().unwrap();
                // info!("time: {:?}:{:?}", then.minute(), then.second());
            }
            Events::Button(ButtonEvent::Released) => {
                info!("Button released event received");
            }
            Events::TempReading(temperature) => {
                info!("Time: {}, TAMB: {} °C, TVC: {} °C", timestamp.get_uptime_seconds(), temperature.0, temperature.1);
                info!("{=str}", Timestamp::seconds_to_iso8601_duration(timestamp.get_uptime_seconds()));
            }
        }

    }
}

#[embassy_executor::task]
async fn button(mut btn: ExtiInput<'static>, msg: Sender<'static, ThreadModeRawMutex, Events, 8>) {
    loop {
        btn.wait_for_falling_edge().await;
        info!("Button pressed!");
        msg.send(Events::Button(ButtonEvent::Pressed)).await;
        // Debounce delay
        Timer::after(Duration::from_millis(50)).await;
        // Wait for release (rising edge)
        btn.wait_for_rising_edge().await;
        info!("Button released!");
        msg.send(Events::Button(ButtonEvent::Released)).await;
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
    msg: Sender<'static, ThreadModeRawMutex, Events, 8>,
) {
    let mut ticker = Ticker::every(Duration::from_secs(10)); // Read every 10 seconds
    loop {
        match temp_sensor.read_temperature_celsius().await {
            Ok(ftemp) => {
                // info!("Temperature: {} °C", ftemp);
                msg.send(Events::TempReading(ftemp)).await;
            }
            Err(_) => warn!("Failed to read from temperature sensor"),
        }
        ticker.next().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use embassy_stm32::rtc::DateTime;

    #[test]
    fn test_seconds_to_datetime() {
        // Example: 0 seconds since epoch should be March 1, 2000, 00:00:00
        let dt = seconds_to_datetime(0);
        assert_eq!(dt.year(), 2000);
        assert_eq!(dt.month(), 3);
        assert_eq!(dt.day(), 1);
        assert_eq!(dt.hour(), 0);
        assert_eq!(dt.minute(), 0);
        assert_eq!(dt.second(), 0);

        // Example: 86400 seconds (1 day) since epoch should be March 2, 2000
        let dt = seconds_to_datetime(86400);
        assert_eq!(dt.year(), 2000);
        assert_eq!(dt.month(), 3);
        assert_eq!(dt.day(), 2);
    }
}
