//! This module contains the business logic for aggregating temperature
//! data from two sensors: one for ambient temperature and another for vaccine storage.
//! It also computes alarms based on the temperature readings.

use crate::timestamp::{Timestamp, TimestampError};

// Constants
pub const MAX_GOOD_VACCINE_TEMP: f32 = 8.0;       // in °C
pub const MIN_GOOD_VACCINE_TEMP: f32 = 2.0;       // in °C
pub const ALARM_HIGH_TEMPERATURE: f32 = 8.0;      // in °C
pub const ALARM_LOW_TEMPERATURE: f32 = -0.5;      // in °C
pub const ALARM_HYSTERESIS: f32 = 0.1;            // in °C
pub const ALARM_HIGH_SECONDS: u32 = 10 * 60 * 60; // 10 hours
pub const ALARM_LOW_SECONDS: u32 = 60 * 60;       // 1 hour


// Structs to hold data

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TemperatureSample {
    pub ambient: Option<f32>,
    pub vaccine: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TempLongRecord {
    pub tvc_sum: f32, // Sum of all vaccine temperatures * their time intervals.   Divided by vaccine_temp_seconds, this gives a time-weighted average vaccine temperature.
    pub tvc_seconds: u32, // Sum of all vaccine temperature measurement time intervals in this record in seconds.
    pub tvc_min: f32, // Minimum vaccine temperature for this record.
    pub tvc_max: f32, // Maximum vaccine temperature for this record.
    pub tamb_sum: f32, // Sum of all ambient temperatures * their time intervals.  Divided by ambient_temp_secons, this gives a time-weighted average ambient temperature.
    pub tamb_seconds: u32, // Sum of all ambient temperature measurement time intervals in this record in seconds.
    pub tvc_low_seconds: u32, // Number of seconds that the vaccine temperature has been less than 2.0 degrees C for this record.
    pub tvc_high_seconds: u32, // Number of seconds thata the vaccine temperature has been greater than 8.0 degrees C for this record.
    pub low_alarm_seconds: u32, // Number of seconds that a low temperature alarm has been in effect for this record.
    pub high_alarm_seconds: u32, // Number of seconds that a high temperature alarm hwas been in effect for this record.
}

// Structs and enums for internal logic.

// Holds cold event start timestamps.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ColdTS {
    cool_start: Timestamp, // Timestamp when temperature went < +2°C.
    freeze_start: Timestamp, // Timestamp when temperature went < -0.5°C.
}

// For the temperature alarm state machine.
#[derive(Debug, Clone, Copy, PartialEq)]
enum AlarmState {
    InRange,
    HotNoAlarm(Timestamp), // Timestamp when temperature went > +8°C.
    HotAlarm(Timestamp),   // Timestamp when temperature went > +8°C.
    Cold(Timestamp),       // Timestamp when temperature went < +2°C.
    FreezeNoAlarm(ColdTS),
    FreezeAlarm(ColdTS),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TemperatureAggregator {
    status: AlarmState,
    last_short_sample_ts: Timestamp, // Timestamp of the last short sample received.
    prev_long_sample_ended: Timestamp, // Timestamp the previous long sampling period ended (just before the start of the current record).
    last_ambient_temp: Option<f32>, // The last good ambient temperature received.
    last_vaccine_temp: Option<f32>, // The last good vaccine temperature received.
    last_ambient_ts: Option<Timestamp>, // The timestamp of the last good ambient temperature.
    last_vaccine_ts: Option<Timestamp>, // The timestamp of the last good vaccine temperature.
    low_alarm_start: Option<Timestamp>, // If alarming, when the alarm started.
    high_alarm_start: Option<Timestamp>, // If alarming, when the alarm started.
    long_record: TempLongRecord, // The long aggregation record currently in process.
}

impl TemperatureAggregator {
    /// Create a new temperature aggregator from the present temperatures and 
    /// timestamp.
    pub fn new(temps: TemperatureSample, now: Timestamp) -> Self {
        // Initialize the "sample_ended" trackers with the last instance before now.
        let last_short_sample_ts = now.get_last_short_sample_end();
        let prev_long_sample_ended = now.get_last_long_sample_end();
        let long_record = TempLongRecord::default();

        // Destructure temperature sample.
        let TemperatureSample{ambient: last_ambient_temp, vaccine: last_vaccine_temp} = temps;

        // Create temperature-related timestamps.
        let last_ambient_ts = last_ambient_temp.map(|_x| {now});
        let last_vaccine_ts = last_vaccine_temp.map(|_x| {now});

        // Get the vaccine temperature, or make an assumption that TVC is room temperature
        // if not availabe, just for initialization purposes.
        let tvc_value = if let Some(tvc_value) = last_vaccine_temp {
            tvc_value
        } else {
            // Room temperature puts us in HotNoAlarm to start.
            23.0
        };

        // Determine status based on vaccine temperature.
        let status = if tvc_value > MAX_GOOD_VACCINE_TEMP {
            AlarmState::HotNoAlarm(now)
        } else if tvc_value < MIN_GOOD_VACCINE_TEMP && tvc_value > ALARM_LOW_TEMPERATURE {
            AlarmState::Cold(now)
        } else {
            AlarmState::FreezeNoAlarm(ColdTS { cool_start: now, freeze_start: now })
        };

        TemperatureAggregator { 
            status, 
            last_short_sample_ts, 
            prev_long_sample_ended, 
            last_ambient_temp, 
            last_vaccine_temp, 
            last_ambient_ts, 
            last_vaccine_ts, 
            low_alarm_start: None, 
            high_alarm_start: None, 
            long_record,
        }
    }

    /// Add a temperature sample to the aggregator.
    pub fn add_sample(&mut self, temps: TemperatureSample, now: Timestamp) {

    }

    /// Determine if there is a high temperature alarm in progress as of
    /// the most recent sample (using its timestamp).  It is best to call
    /// this function shortly after adding a sample with add_sample().
    pub fn is_low_alarm(&self) -> bool {
        false
    }

    /// Determine if there is a high temperature alarm in progress as of
    /// the most recent sample (using its timestamp).  It is best to call
    /// this function shortly after adding a sample with add_sample().
    pub fn is_high_alarm(&self) -> bool {
        false
    }

    /// Returns the data in the present long record so that it may
    /// be saved. Also resets the record for future aggregation.
    pub fn finalize_long_record(&mut self) -> TempLongRecord {
        TempLongRecord::default()
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    // Test for 11 hours at 8.0°C; it should not alarm.
    #[test]
    fn test_edge_of_hot() {
        // Initialize at 0 sec with vaccine temp 4.0°C, ambient 25.0°C
        let t0 = Timestamp { seconds: 0 };
        let initial_sample = TemperatureSample { ambient: Some(25.0), vaccine: Some(4.0) };
        let mut agg = TemperatureAggregator::new(initial_sample, t0);

        // At 900 sec, vaccine temp rises to 8.0°C, ambient stays 25.0°C
        let t1 = Timestamp { seconds: 900 };
        let sample_1 = TemperatureSample { ambient: Some(25.0), vaccine: Some(8.0) };
        agg.add_sample(sample_1, t1);

        // Check that the last_vaccine_temp and last_ambient_temp are updated
        assert_eq!(agg.last_vaccine_temp, Some(8.0));
        assert_eq!(agg.last_ambient_temp, Some(25.0));

        // Check that the last_vaccine_ts and last_ambient_ts are updated
        assert_eq!(agg.last_vaccine_ts, Some(t1));
        assert_eq!(agg.last_ambient_ts, Some(t1));

        // Check long record aggregations.
        assert_eq!(agg.long_record.tvc_min, 4.0);
        assert_eq!(agg.long_record.tvc_max, 8.0);
        let tvc_check: f32 = (4.0 + 8.0) / 2.0 * 900.0;
        assert_eq!(agg.long_record.tvc_sum, tvc_check); // Trapezoidal averaging
        assert_eq!(agg.long_record.tvc_seconds, 900);
        assert_eq!(agg.long_record.tvc_low_seconds, 0);
        assert_eq!(agg.long_record.tvc_high_seconds, 0);
        assert_eq!(agg.long_record.high_alarm_seconds, 0);
        assert_eq!(agg.long_record.low_alarm_seconds, 0);
        assert_eq!(agg.long_record.tamb_seconds, 900);
        let amb_check: f32 = (17.0 + 25.0) / 2.0 * 900.0; // Trapezoidal
        assert_eq!(agg.long_record.tamb_sum, amb_check);

        // Check that the status is InRange
        match agg.status {
            AlarmState::InRange => (),
            _ => panic!("Expected InRange state because vaccine temp has not exceeded 8.0°C"),
        }

        // Check that it is not alarming.
        assert!(!agg.is_high_alarm());
        assert!(!agg.is_low_alarm());

        // At 1800 sec, vaccine temp still at 8.0°C, ambient drops to 17.0°C
        let t2 = Timestamp { seconds: 1800 };
        let sample_2 = TemperatureSample { ambient: Some(17.0), vaccine: Some(8.0) };
        agg.add_sample(sample_2, t2);

        // At 28800 sec (8h), vaccine temp still at 8.0°C, ambient still at 17.0°C
        let t3 = Timestamp { seconds: 28_800 };
        agg.add_sample(sample_2, t3);

        // At 39600 sec (11h), vaccine temp still at 8.0°C, ambient still at 17.0°C
        let t3 = Timestamp { seconds: 39_600 };
        agg.add_sample(sample_2, t3);

        // Check long record aggregations.
        assert_eq!(agg.long_record.tvc_min, 4.0);
        assert_eq!(agg.long_record.tvc_max, 8.0);
        let tvc_check: f32 = tvc_check + 8.0 * (39_600.0 - 900.0); // Triangular averaging
        assert_eq!(agg.long_record.tvc_sum, tvc_check);
        assert_eq!(agg.long_record.tvc_seconds, 39_600);
        assert_eq!(agg.long_record.tvc_low_seconds, 0);
        assert_eq!(agg.long_record.tvc_high_seconds, 0);
        assert_eq!(agg.long_record.high_alarm_seconds, 0);
        assert_eq!(agg.long_record.low_alarm_seconds, 0);
        assert_eq!(agg.long_record.tamb_seconds, 39_600);
        let amb_check: f32 = amb_check + (39_600.0 - 900.0) * 17.0;
        assert_eq!(agg.long_record.tamb_sum, amb_check);

        // Check that the status is InRange
        match agg.status {
            AlarmState::InRange => (),
            _ => panic!("Expected InRange state because vaccine temp has not exceeded 8.0°C"),
        }

        // Check that it is not alarming.
        assert!(!agg.is_high_alarm());
        assert!(!agg.is_low_alarm());
    }

    // Test for 15 minutes at 4°C and 10.75 hours at 8.01°C; it should alarm at 10 hours and have 45min = 2700 sec of alarm after 11h.
    #[test]
    fn test_hot() {
        // Initialize at 0 sec with vaccine temp 4.0°C, ambient 25.0°C
        let t0 = Timestamp { seconds: 0 };
        let initial_sample = TemperatureSample { ambient: Some(25.0), vaccine: Some(4.0) };
        let mut agg = TemperatureAggregator::new(initial_sample, t0);

        // At 900 sec, vaccine temp rises to 8.01°C, ambient stays 25.0°C
        let t1 = Timestamp { seconds: 900 };
        let sample_1 = TemperatureSample { ambient: Some(25.0), vaccine: Some(8.01) };
        agg.add_sample(sample_1, t1);

        // Check that the last_vaccine_temp and last_ambient_temp are updated
        assert_eq!(agg.last_vaccine_temp, Some(8.01));
        assert_eq!(agg.last_ambient_temp, Some(25.0));

        // Check that the last_vaccine_ts and last_ambient_ts are updated
        assert_eq!(agg.last_vaccine_ts, Some(t1));
        assert_eq!(agg.last_ambient_ts, Some(t1));

        // Check long record aggregations.
        assert_eq!(agg.long_record.tvc_min, 4.0);
        assert_eq!(agg.long_record.tvc_max, 8.01);
        let tvc_check: f32 = (4.0 + 8.01) / 2.0 * 900.0;
        assert_eq!(agg.long_record.tvc_sum, tvc_check); // Trapezoidal averaging
        assert_eq!(agg.long_record.tvc_seconds, 900);
        assert_eq!(agg.long_record.tvc_low_seconds, 0);
        assert_eq!(agg.long_record.tvc_high_seconds, 0);
        assert_eq!(agg.long_record.high_alarm_seconds, 0);
        assert_eq!(agg.long_record.low_alarm_seconds, 0);
        assert_eq!(agg.long_record.tamb_seconds, 900);
        let amb_check: f32 = 25.0 * 900.0;
        assert_eq!(agg.long_record.tamb_sum, amb_check);

        // Check that the status is InRange
        match agg.status {
            AlarmState::HotNoAlarm(Timestamp{seconds}) => {
                assert_eq!(seconds, 900);
            },
            _ => panic!("Expected HotNoAlarm state because vaccine temp > 8.0°C"),
        }

        // Check that it is not alarming.
        assert!(!agg.is_high_alarm());
        assert!(!agg.is_low_alarm());

        // At 1800 sec, vaccine temp still at 8.01°C, ambient drops to 17.0°C
        let t2 = Timestamp { seconds: 1800 };
        let sample_2 = TemperatureSample { ambient: Some(17.0), vaccine: Some(8.01) };
        agg.add_sample(sample_2, t2);

        // We are now in high temperature territory.
        assert_eq!(agg.long_record.tvc_high_seconds, 0);
        // Ambient accumulation includes a constant part and a trapezoedal part.
        let amb_check: f32 = amb_check + (17.0 + 25.0) / 2.0 * 900.0;
        assert_eq!(agg.long_record.tamb_sum, amb_check);


        // At 28800 sec (8h), vaccine temp still at 8.01°C, ambient still at 17.0°C
        let t3 = Timestamp { seconds: 28_800 };
        agg.add_sample(sample_2, t3);

        // At 39600 sec (11h), vaccine temp still at 8.01°C, ambient still at 17.0°C
        let t3 = Timestamp { seconds: 39_600 };
        agg.add_sample(sample_2, t3);

        // Check long record aggregations.
        assert_eq!(agg.long_record.tvc_min, 4.0);
        assert_eq!(agg.long_record.tvc_max, 8.01);
        let tvc_check: f32 = tvc_check + 8.1 * (39_600.0 - 900.0);
        assert_eq!(agg.long_record.tvc_sum, tvc_check);
        assert_eq!(agg.long_record.tvc_seconds, 39_600);
        assert_eq!(agg.long_record.tvc_low_seconds, 0);
        assert_eq!(agg.long_record.tvc_high_seconds, (39_600 - 900));
        assert_eq!(agg.long_record.high_alarm_seconds, (3600 - 900));
        assert_eq!(agg.long_record.low_alarm_seconds, 0);
        assert_eq!(agg.long_record.tamb_seconds, 39_600);
        let amb_check: f32 = amb_check + (39_600.0 - 1800.0) * 17.0;
        assert_eq!(agg.long_record.tamb_sum, amb_check);

        // Check that the status is InRange
        match agg.status {
            AlarmState::HotAlarm(Timestamp{seconds}) => {
                assert_eq!(seconds, 900);
            },
            _ => panic!("Expected Hot Alarm because TVC > 8.0°C for t > 10h"),
        }

        // Check that it is high alarming.
        assert!(agg.is_high_alarm());
        assert!(!agg.is_low_alarm());
    }
    
    // Test for 2 hours at 2.0°C; it should not go into the "Cold" state.
    #[test]
    fn test_edge_of_cool() {
        // Initialize at 0 sec with vaccine temp 4.0°C, ambient 25.0°C
        let t0 = Timestamp { seconds: 0 };
        let initial_sample = TemperatureSample { ambient: Some(25.0), vaccine: Some(4.0) };
        let mut agg = TemperatureAggregator::new(initial_sample, t0);

        // At 900 sec, vaccine temp drops to 2.0°C, ambient stays 25.0°C
        let t1 = Timestamp { seconds: 900 };
        let sample_1 = TemperatureSample { ambient: Some(25.0), vaccine: Some(2.0) };
        agg.add_sample(sample_1, t1);

        // Check that the last_vaccine_temp and last_ambient_temp are updated
        assert_eq!(agg.last_vaccine_temp, Some(2.0));
        assert_eq!(agg.last_ambient_temp, Some(25.0));

        // Check that the last_vaccine_ts and last_ambient_ts are updated
        assert_eq!(agg.last_vaccine_ts, Some(t1));
        assert_eq!(agg.last_ambient_ts, Some(t1));

        // Check long record aggregations.
        assert_eq!(agg.long_record.tvc_min, 2.0);
        assert_eq!(agg.long_record.tvc_max, 4.0);
        let tvc_check: f32 = (4.0 + 2.0) / 2.0 * 900.0;
        assert_eq!(agg.long_record.tvc_sum, tvc_check); // Trapezoidal averaging
        assert_eq!(agg.long_record.tvc_seconds, 900);
        assert_eq!(agg.long_record.tvc_low_seconds, 0);
        assert_eq!(agg.long_record.tvc_high_seconds, 0);
        assert_eq!(agg.long_record.high_alarm_seconds, 0);
        assert_eq!(agg.long_record.low_alarm_seconds, 0);
        assert_eq!(agg.long_record.tamb_seconds, 900);
        let amb_check: f32 = 25.0 * 900.0;
        assert_eq!(agg.long_record.tamb_sum, amb_check);

        // Check that the status is InRange
        match agg.status {
            AlarmState::InRange => (),
            _ => panic!("Expected InRange state because vaccine temp has not dropped below 2.0°C"),
        }

        // Check that it is not alarming.
        assert!(!agg.is_high_alarm());
        assert!(!agg.is_low_alarm());

            // At 7200 sec, vaccine temp still at 2.0°C, ambient stays the same.
        let t2 = Timestamp { seconds: 7200 };
        let sample_2 = TemperatureSample { ambient: Some(25.0), vaccine: Some(2.0) };
        agg.add_sample(sample_2, t2);

        // Check long record aggregations.
        assert_eq!(agg.long_record.tvc_min, 2.0);
        assert_eq!(agg.long_record.tvc_max, 8.0);
        let tvc_check: f32 = tvc_check + 2.0 * (7200.0 - 900.0); // Triangular averaging
        assert_eq!(agg.long_record.tvc_sum, tvc_check);
        assert_eq!(agg.long_record.tvc_seconds, 7200);
        assert_eq!(agg.long_record.tvc_low_seconds, 0);
        assert_eq!(agg.long_record.tvc_high_seconds, 0);
        assert_eq!(agg.long_record.high_alarm_seconds, 0);
        assert_eq!(agg.long_record.low_alarm_seconds, 0);
        assert_eq!(agg.long_record.tamb_seconds, 7200);
        let amb_check: f32 = amb_check + (7200.0 - 900.0) * 25.0;
        assert_eq!(agg.long_record.tamb_sum, amb_check);

        // Check that the status is InRange
        match agg.status {
            AlarmState::InRange => (),
            _ => panic!("Expected InRange state because vaccine temp has not exceeded 8.0°C"),
        }

        // Check that it is not alarming.
        assert!(!agg.is_high_alarm());
        assert!(!agg.is_low_alarm());
    }

}
