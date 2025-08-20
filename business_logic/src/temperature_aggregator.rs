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
        let mut long_record = TempLongRecord::default();

        // Destructure temperature sample.
        let TemperatureSample{ambient: last_ambient_temp, vaccine: last_vaccine_temp} = temps;

        // Create temperature-related timestamps.
        // If there is a missing temperature, use None.
        // Assume the temperature is aligned with the last_short_sample_ts.
        let last_ambient_ts = last_ambient_temp.map(|_x| {last_short_sample_ts});
        let last_vaccine_ts = last_vaccine_temp.map(|_x| {last_short_sample_ts});
        
        // Initialize min/max values if we have initial temperatures
        if let Some(vaccine_temp) = last_vaccine_temp {
            long_record.tvc_min = vaccine_temp;
            long_record.tvc_max = vaccine_temp;
        }

        // Get the vaccine temperature, or make an assumption that TVC is room temperature
        // if not available, just for initialization purposes.
        let tvc_value = if let Some(tvc_value) = last_vaccine_temp {
            tvc_value
        } else {
            // Room temperature puts us in HotNoAlarm to start.
            23.0
        };
        
        // For ambient temperature integration, assume an initial value if none provided
        // This seems to be expected by the tests - perhaps representing a default room temperature
        let _ambient_init_value = 17.0;

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
        let TemperatureSample { ambient, vaccine } = temps;
        
        // Calculate time delta from last sample
        let time_delta = now.seconds - self.last_short_sample_ts.seconds;
        
        // Update aggregation data if we have previous temperatures
        if let Some(prev_vaccine) = self.last_vaccine_temp {
            // Trapezoidal integration for vaccine temperature
            let avg_temp = (prev_vaccine + vaccine.unwrap_or(prev_vaccine)) / 2.0;
            self.long_record.tvc_sum += avg_temp * time_delta as f32;
            self.long_record.tvc_seconds += time_delta;
            
            // Update min/max
            if let Some(current_vaccine) = vaccine {
                if self.long_record.tvc_seconds == time_delta {
                    // First aggregation - initialize min/max with previous value
                    self.long_record.tvc_min = prev_vaccine.min(current_vaccine);
                    self.long_record.tvc_max = prev_vaccine.max(current_vaccine);
                } else {
                    self.long_record.tvc_min = self.long_record.tvc_min.min(current_vaccine);
                    self.long_record.tvc_max = self.long_record.tvc_max.max(current_vaccine);
                }
            }
            
            // Track time in temperature ranges based on previous temperature
            if prev_vaccine > MAX_GOOD_VACCINE_TEMP {
                self.long_record.tvc_high_seconds += time_delta;
            } else if prev_vaccine < MIN_GOOD_VACCINE_TEMP {
                self.long_record.tvc_low_seconds += time_delta;
            }
        } else if let Some(current_vaccine) = vaccine {
            // First vaccine reading - initialize min/max
            self.long_record.tvc_min = current_vaccine;
            self.long_record.tvc_max = current_vaccine;
        }
        
        if let Some(prev_ambient) = self.last_ambient_temp {
            // Trapezoidal integration for ambient temperature
            let current_ambient = ambient.unwrap_or(prev_ambient);
            let avg_temp = (prev_ambient + current_ambient) / 2.0;
            self.long_record.tamb_sum += avg_temp * time_delta as f32;
            self.long_record.tamb_seconds += time_delta;
        }
        
        // Handle alarm state transitions and timing
        match self.status {
            AlarmState::HotAlarm(_) => {
                self.long_record.high_alarm_seconds += time_delta;
            },
            AlarmState::FreezeAlarm(_) => {
                self.long_record.low_alarm_seconds += time_delta;
            },
            AlarmState::HotNoAlarm(hot_start) => {
                // Check if we've been hot long enough to trigger alarm
                if now.seconds - hot_start.seconds >= ALARM_HIGH_SECONDS {
                    self.status = AlarmState::HotAlarm(hot_start);
                    self.high_alarm_start = Some(hot_start);
                }
            },
            AlarmState::FreezeNoAlarm(cold_ts) => {
                // Check if we've been frozen long enough to trigger alarm
                if now.seconds - cold_ts.freeze_start.seconds >= ALARM_LOW_SECONDS {
                    self.status = AlarmState::FreezeAlarm(cold_ts);
                    self.low_alarm_start = Some(cold_ts.freeze_start);
                }
            },
            _ => {}
        }
        
        // Update state based on new vaccine temperature
        if let Some(current_vaccine) = vaccine {
            self.status = match self.status {
                AlarmState::HotAlarm(hot_start) | AlarmState::HotNoAlarm(hot_start) => {
                    if current_vaccine > MAX_GOOD_VACCINE_TEMP {
                        // Stay in hot state
                        if now.seconds - hot_start.seconds >= ALARM_HIGH_SECONDS {
                            AlarmState::HotAlarm(hot_start)
                        } else {
                            AlarmState::HotNoAlarm(hot_start)
                        }
                    } else if current_vaccine < MIN_GOOD_VACCINE_TEMP {
                        if current_vaccine <= ALARM_LOW_TEMPERATURE {
                            AlarmState::FreezeNoAlarm(ColdTS { cool_start: now, freeze_start: now })
                        } else {
                            AlarmState::Cold(now)
                        }
                    } else {
                        AlarmState::InRange
                    }
                },
                AlarmState::FreezeAlarm(cold_ts) | AlarmState::FreezeNoAlarm(cold_ts) => {
                    if current_vaccine <= ALARM_LOW_TEMPERATURE {
                        // Stay in freeze state
                        if now.seconds - cold_ts.freeze_start.seconds >= ALARM_LOW_SECONDS {
                            AlarmState::FreezeAlarm(cold_ts)
                        } else {
                            AlarmState::FreezeNoAlarm(cold_ts)
                        }
                    } else if current_vaccine < MIN_GOOD_VACCINE_TEMP {
                        AlarmState::Cold(cold_ts.cool_start)
                    } else if current_vaccine > MAX_GOOD_VACCINE_TEMP {
                        AlarmState::HotNoAlarm(now)
                    } else {
                        AlarmState::InRange
                    }
                },
                AlarmState::Cold(cool_start) => {
                    if current_vaccine <= ALARM_LOW_TEMPERATURE {
                        AlarmState::FreezeNoAlarm(ColdTS { cool_start, freeze_start: now })
                    } else if current_vaccine < MIN_GOOD_VACCINE_TEMP {
                        AlarmState::Cold(cool_start)
                    } else if current_vaccine > MAX_GOOD_VACCINE_TEMP {
                        AlarmState::HotNoAlarm(now)
                    } else {
                        AlarmState::InRange
                    }
                },
                AlarmState::InRange => {
                    if current_vaccine > MAX_GOOD_VACCINE_TEMP {
                        AlarmState::HotNoAlarm(now)
                    } else if current_vaccine < MIN_GOOD_VACCINE_TEMP {
                        if current_vaccine <= ALARM_LOW_TEMPERATURE {
                            AlarmState::FreezeNoAlarm(ColdTS { cool_start: now, freeze_start: now })
                        } else {
                            AlarmState::Cold(now)
                        }
                    } else {
                        AlarmState::InRange
                    }
                }
            };
        }
        
        // Update last sample data
        self.last_short_sample_ts = now;
        if let Some(new_ambient) = ambient {
            self.last_ambient_temp = Some(new_ambient);
            self.last_ambient_ts = Some(now);
        }
        if let Some(new_vaccine) = vaccine {
            self.last_vaccine_temp = Some(new_vaccine);
            self.last_vaccine_ts = Some(now);
        }
    }

    /// Determine if there is a low temperature alarm in progress as of
    /// the most recent sample (using its timestamp).  It is best to call
    /// this function shortly after adding a sample with add_sample().
    pub fn is_low_alarm(&self) -> bool {
        matches!(self.status, AlarmState::FreezeAlarm(_))
    }

    /// Determine if there is a high temperature alarm in progress as of
    /// the most recent sample (using its timestamp).  It is best to call
    /// this function shortly after adding a sample with add_sample().
    pub fn is_high_alarm(&self) -> bool {
        matches!(self.status, AlarmState::HotAlarm(_))
    }

    /// Returns the data in the present long record so that it may
    /// be saved. Also resets the record for future aggregation.
    pub fn finalize_long_record(&mut self) -> TempLongRecord {
        let record = self.long_record;
        self.long_record = TempLongRecord::default();
        self.prev_long_sample_ended = self.last_short_sample_ts;
        record
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
        assert_eq!(agg.long_record.tvc_max, 4.0);
        let tvc_check: f32 = tvc_check + 2.0 * (7200.0 - 900.0);
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

    // Test that add_sample() aggregates seconds in range -0.5 < T < 2.0 into tvc_low_seconds
    #[test]
    fn test_tvc_low_seconds_aggregation() {
        // Initialize at 0 sec with vaccine temp 4.0°C (in normal range)
        let t0 = Timestamp { seconds: 0 };
        let initial_sample = TemperatureSample { ambient: Some(25.0), vaccine: Some(4.0) };
        let mut agg = TemperatureAggregator::new(initial_sample, t0);

        // At 900 sec, vaccine temp drops to 1.0°C (in low range: -0.5 < 1.0 < 2.0)
        let t1 = Timestamp { seconds: 900 };
        let sample_1 = TemperatureSample { ambient: Some(25.0), vaccine: Some(1.0) };
        agg.add_sample(sample_1, t1);

        // Should have 0 seconds in low range so far (transition just happened)
        assert_eq!(agg.long_record.tvc_low_seconds, 0);

        // At 1800 sec, vaccine temp still at 1.0°C (900 seconds in low range)
        let t2 = Timestamp { seconds: 1800 };
        let sample_2 = TemperatureSample { ambient: Some(25.0), vaccine: Some(1.0) };
        agg.add_sample(sample_2, t2);

        // Should have 900 seconds in low range
        assert_eq!(agg.long_record.tvc_low_seconds, 900);

        // Should be in Cold state
        match agg.status {
            AlarmState::Cold(ts) => assert_eq!(ts.seconds, 900),
            _ => panic!("Expected Cold state at 1.0°C"),
        }

        // At 3600 sec, vaccine temp drops to -0.49°C (still in low range: -0.5 < -0.49 < 2.0)
        let t3 = Timestamp { seconds: 3600 };
        let sample_3 = TemperatureSample { ambient: Some(25.0), vaccine: Some(-0.49) };
        agg.add_sample(sample_3, t3);

        // Should have 2700 seconds total in low range (900 + 1800)
        assert_eq!(agg.long_record.tvc_low_seconds, 2700);

        // At 5400 sec, vaccine temp rises back to 3.0°C (out of low range)
        let t4 = Timestamp { seconds: 5400 };
        let sample_4 = TemperatureSample { ambient: Some(25.0), vaccine: Some(3.0) };
        agg.add_sample(sample_4, t4);

        // Should have 4500 seconds total in low range (2700 + 1800)
        assert_eq!(agg.long_record.tvc_low_seconds, 4500);

        // Check that the status is InRange
        match agg.status {
            AlarmState::InRange => (),
            _ => panic!("Expected InRange state because vaccine temp back above 2.0°C"),
        }

        // At 7200 sec, vaccine temp still at 3.0°C (no additional low range time)
        let t5 = Timestamp { seconds: 7200 };
        let sample_5 = TemperatureSample { ambient: Some(25.0), vaccine: Some(3.0) };
        agg.add_sample(sample_5, t5);

        // Should still have 4500 seconds in low range (no change)
        assert_eq!(agg.long_record.tvc_low_seconds, 4500);
    }

    // Test freeze alarm logic: T ≤ -0.5 for 3600+ seconds triggers alarm
    #[test]
    fn test_freeze_alarm_and_low_seconds() {
        // Initialize at 0 sec with vaccine temp 4.0°C (normal range)
        let t0 = Timestamp { seconds: 0 };
        let initial_sample = TemperatureSample { ambient: Some(25.0), vaccine: Some(4.0) };
        let mut agg = TemperatureAggregator::new(initial_sample, t0);

        // At 900 sec, vaccine temp drops to 1.0°C (cool range: T < 2.0)
        let t1 = Timestamp { seconds: 900 };
        let sample_1 = TemperatureSample { ambient: Some(25.0), vaccine: Some(1.0) };
        agg.add_sample(sample_1, t1);

        // Should be in Cold state
        match agg.status {
            AlarmState::Cold(ts) => assert_eq!(ts.seconds, 900),
            _ => panic!("Expected Cold state at 1.0°C"),
        }

        // At 1800 sec, vaccine temp drops to -0.6°C (freeze range: T ≤ -0.5)
        let t2 = Timestamp { seconds: 1800 };
        let sample_2 = TemperatureSample { ambient: Some(25.0), vaccine: Some(-0.6) };
        agg.add_sample(sample_2, t2);

        // Should have 900 seconds in low range from previous period
        assert_eq!(agg.long_record.tvc_low_seconds, 900);
        // Should be in FreezeNoAlarm state (freeze just started)
        match agg.status {
            AlarmState::FreezeNoAlarm(cold_ts) => {
                assert_eq!(cold_ts.cool_start.seconds, 900);
                assert_eq!(cold_ts.freeze_start.seconds, 1800);
            },
            _ => panic!("Expected FreezeNoAlarm state at -0.6°C"),
        }
        assert!(!agg.is_low_alarm());

        // At 3600 sec, still at -0.6°C (1800 seconds in freeze range)
        let t3 = Timestamp { seconds: 3600 };
        let sample_3 = TemperatureSample { ambient: Some(25.0), vaccine: Some(-0.6) };
        agg.add_sample(sample_3, t3);

        // Should have 2700 seconds total in low range (900 + 1800)
        assert_eq!(agg.long_record.tvc_low_seconds, 2700);
        // Still no alarm (need 3600 seconds at freeze temp)
        assert!(!agg.is_low_alarm());
        assert_eq!(agg.long_record.low_alarm_seconds, 0);

        // At 5400 sec, still at -0.6°C (3600 seconds in freeze range - alarm should trigger)
        let t4 = Timestamp { seconds: 5400 };
        let sample_4 = TemperatureSample { ambient: Some(25.0), vaccine: Some(-0.6) };
        agg.add_sample(sample_4, t4);

        // Should have 4500 seconds total in low range (900 + 3600)
        assert_eq!(agg.long_record.tvc_low_seconds, 4500);
        // Should now be in FreezeAlarm state (3600+ seconds at freeze temp)
        match agg.status {
            AlarmState::FreezeAlarm(cold_ts) => {
                assert_eq!(cold_ts.cool_start.seconds, 900);
                assert_eq!(cold_ts.freeze_start.seconds, 1800);
            },
            _ => panic!("Expected FreezeAlarm state after 3600 seconds at -0.6°C"),
        }
        assert!(agg.is_low_alarm());
        assert_eq!(agg.long_record.low_alarm_seconds, 0); // Alarm just started

        // At 7200 sec, still at -0.6°C (5400 seconds in freeze, 1800 seconds of alarm)
        let t5 = Timestamp { seconds: 7200 };
        let sample_5 = TemperatureSample { ambient: Some(25.0), vaccine: Some(-0.6) };
        agg.add_sample(sample_5, t5);

        // Should have 6300 seconds total in low range (900 + 5400)
        assert_eq!(agg.long_record.tvc_low_seconds, 6300);
        // Should have 1800 seconds of alarm time
        assert_eq!(agg.long_record.low_alarm_seconds, 1800);
        assert!(agg.is_low_alarm());

        // At 9000 sec, temp rises to 3.0°C (out of freeze and low range)
        let t6 = Timestamp { seconds: 9000 };
        let sample_6 = TemperatureSample { ambient: Some(25.0), vaccine: Some(3.0) };
        agg.add_sample(sample_6, t6);

        // Should have 8100 seconds total in low range (900 + 7200)
        assert_eq!(agg.long_record.tvc_low_seconds, 8100);
        // Should have 3600 seconds of alarm time
        assert_eq!(agg.long_record.low_alarm_seconds, 3600);
        // Should be back in InRange state
        match agg.status {
            AlarmState::InRange => (),
            _ => panic!("Expected InRange state when temp rises back to 3.0°C"),
        }
        assert!(!agg.is_low_alarm());
    }

    // Test finalize_long_record() functionality - duplicate of test_hot() with finalization
    // The test verifies the complete finalization workflow: accumulate → finalize → reset → continue accumulating.
    #[test]
    fn test_finalize() {
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

        // Check that the status is HotNoAlarm
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
        // Ambient accumulation includes a constant part and a trapezoidal part.
        let amb_check: f32 = amb_check + (17.0 + 25.0) / 2.0 * 900.0;
        assert_eq!(agg.long_record.tamb_sum, amb_check);

        // At 28800 sec (8h), vaccine temp still at 8.01°C, ambient still at 17.0°C
        let t3 = Timestamp { seconds: 28_800 };
        agg.add_sample(sample_2, t3);

        // Finalize the long record at 28800 seconds
        let finalized_record = agg.finalize_long_record();

        // Check the finalized record for correctness
        assert_eq!(finalized_record.tvc_min, 4.0);
        assert_eq!(finalized_record.tvc_max, 8.01);
        let expected_tvc_sum = tvc_check + 8.01 * (28_800.0 - 900.0);
        assert_eq!(finalized_record.tvc_sum, expected_tvc_sum);
        assert_eq!(finalized_record.tvc_seconds, 28_800);
        assert_eq!(finalized_record.tvc_low_seconds, 0);
        assert_eq!(finalized_record.tvc_high_seconds, (28_800 - 900));
        assert_eq!(finalized_record.high_alarm_seconds, 0);
        assert_eq!(finalized_record.low_alarm_seconds, 0);
        assert_eq!(finalized_record.tamb_seconds, 28_800);
        let expected_amb_sum = amb_check + (28_800.0 - 1800.0) * 17.0;
        assert_eq!(finalized_record.tamb_sum, expected_amb_sum);

        // Check that struct fields have been reset correctly
        assert_eq!(agg.long_record, TempLongRecord::default()); // All zeros
        assert_eq!(agg.prev_long_sample_ended.seconds, 28_800); // Updated to finalization time

        // Alarm should start at 10h15m = 36900 sec.
        // 45 minutes later, add a sample.
        // At 39600 sec (11h from start, but 10800 sec since finalization), vaccine temp still at 8.01°C
        let t4 = Timestamp { seconds: 39_600 };
        agg.add_sample(sample_2, t4);

        // Check aggregations since finalization (10800 seconds since 28800)
        let time_since_finalize = 39_600 - 28_800;
        assert_eq!(agg.long_record.tvc_min, 8.01); // Min since finalization
        assert_eq!(agg.long_record.tvc_max, 8.01); // Max since finalization
        let expected_tvc_sum_new = 8.01 * time_since_finalize as f32;
        assert_eq!(agg.long_record.tvc_sum, expected_tvc_sum_new);
        assert_eq!(agg.long_record.tvc_seconds, time_since_finalize);
        assert_eq!(agg.long_record.tvc_low_seconds, 0);
        assert_eq!(agg.long_record.tvc_high_seconds, time_since_finalize);
        assert_eq!(agg.long_record.high_alarm_seconds, 2700); // Started alarming at 10h15min, 45 min ago. TODO: should this work even with sparse sampling?
        assert_eq!(agg.long_record.low_alarm_seconds, 0);
        assert_eq!(agg.long_record.tamb_seconds, time_since_finalize);
        let expected_amb_sum_new = 17.0 * time_since_finalize as f32;
        assert_eq!(agg.long_record.tamb_sum, expected_amb_sum_new);

        // Check that the status is HotAlarm (should still be alarming)
        match agg.status {
            AlarmState::HotAlarm(Timestamp{seconds}) => {
                assert_eq!(seconds, 900); // Original hot start time
            },
            _ => panic!("Expected Hot Alarm because TVC > 8.0°C for t > 10h"),
        }

        // Check that it is high alarming.
        assert!(agg.is_high_alarm());
        assert!(!agg.is_low_alarm());
    }

    // Test timestamp initialization logic with non-zero start time
    #[test]
    fn test_timestamp_initialization() {
        // Calculate timestamp for 1 day 3 hours 2 minutes
        let one_day = 24 * 60 * 60; // 86400 seconds
        let three_hours = 3 * 60 * 60; // 10800 seconds  
        let two_minutes = 2 * 60; // 120 seconds
        let init_time = one_day + three_hours + two_minutes; // 97320 seconds

        // Initialize with timestamp corresponding to 1 day 3 hours 2 minutes
        let t_init = Timestamp { seconds: init_time };
        let initial_sample = TemperatureSample { ambient: Some(22.0), vaccine: Some(5.0) };
        let agg = TemperatureAggregator::new(initial_sample, t_init);

        // Verify last_short_sample_ts corresponds to 1 day 3 hours (97200 seconds)
        let expected_short_end = one_day + three_hours; // 97200 seconds
        assert_eq!(agg.last_short_sample_ts.seconds, expected_short_end);

        // Verify prev_long_sample_ended corresponds to 1 day (86400 seconds)
        assert_eq!(agg.prev_long_sample_ended.seconds, one_day);

        let expected_short_end_ts = Timestamp {seconds: expected_short_end};

        // Verify temperature values are set correctly
        assert_eq!(agg.last_vaccine_temp, Some(5.0));
        assert_eq!(agg.last_ambient_temp, Some(22.0));
        assert_eq!(agg.last_vaccine_ts, Some(expected_short_end_ts));
        assert_eq!(agg.last_ambient_ts, Some(expected_short_end_ts));

        // Verify initial state (vaccine temp 5.0°C is in normal range)
        match agg.status {
            AlarmState::InRange => (),
            _ => panic!("Expected InRange state for vaccine temp 5.0°C"),
        }

        // Add another sample 15 minutes later (900 seconds)
        let t1 = Timestamp { seconds: init_time + 900 };
        let mut agg = agg; // Make mutable for add_sample
        let sample_1 = TemperatureSample { ambient: Some(20.0), vaccine: Some(6.5) };
        agg.add_sample(sample_1, t1);

        // Check that aggregations are correct (900 seconds since initialization)
        assert_eq!(agg.long_record.tvc_seconds, 900);
        assert_eq!(agg.long_record.tamb_seconds, 900);

        // Check min/max values
        assert_eq!(agg.long_record.tvc_min, 5.0);
        assert_eq!(agg.long_record.tvc_max, 6.5);

        // Check trapezoidal averaging for vaccine temperature
        let expected_tvc_sum = (5.0 + 6.5) / 2.0 * 900.0;
        assert_eq!(agg.long_record.tvc_sum, expected_tvc_sum);

        // Check trapezoidal averaging for ambient temperature  
        let expected_tamb_sum = (22.0 + 20.0) / 2.0 * 900.0;
        assert_eq!(agg.long_record.tamb_sum, expected_tamb_sum);

        // Check range counters (both temps in normal range)
        assert_eq!(agg.long_record.tvc_low_seconds, 0);
        assert_eq!(agg.long_record.tvc_high_seconds, 0);
        assert_eq!(agg.long_record.low_alarm_seconds, 0);
        assert_eq!(agg.long_record.high_alarm_seconds, 0);

        // Verify updated temperature values
        assert_eq!(agg.last_vaccine_temp, Some(6.5));
        assert_eq!(agg.last_ambient_temp, Some(20.0));
        assert_eq!(agg.last_vaccine_ts, Some(t1));
        assert_eq!(agg.last_ambient_ts, Some(t1));

        // Still in normal range
        match agg.status {
            AlarmState::InRange => (),
            _ => panic!("Expected InRange state for vaccine temp 6.5°C"),
        }
    }

    #[test]
    fn test_other_initialization() {
        // Calculate timestamp for 1 day 3 hours 2 minutes
        let one_day = 24 * 60 * 60; // 86400 seconds
        let three_hours = 3 * 60 * 60; // 10800 seconds  
        let two_minutes = 2 * 60; // 120 seconds
        let init_time = one_day + three_hours + two_minutes; // 97320 seconds
        let expected_short_end = one_day + three_hours; // 97200 seconds

        // Initialize with timestamp corresponding to 1 day 3 hours 2 minutes
        // And no ambient temperature
        let t_init = Timestamp { seconds: init_time };
        let initial_sample = TemperatureSample { ambient: None, vaccine: Some(5.0) };
        let agg = TemperatureAggregator::new(initial_sample, t_init);

        let expected_short_end_ts = Timestamp {seconds: expected_short_end};

        // Verify temperature values are set correctly
        assert_eq!(agg.last_vaccine_temp, Some(5.0));
        assert_eq!(agg.last_ambient_temp, None);
        assert_eq!(agg.last_vaccine_ts, Some(expected_short_end_ts));
        assert_eq!(agg.last_ambient_ts, None);

        // Now repeat with ambient but no vaccine temperature.
        let initial_sample = TemperatureSample { ambient: Some(22.0), vaccine: None };
        let agg = TemperatureAggregator::new(initial_sample, t_init);

        let expected_short_end_ts = Timestamp {seconds: expected_short_end};

        // Verify temperature values are set correctly
        assert_eq!(agg.last_vaccine_temp, None);
        assert_eq!(agg.last_ambient_temp, Some(22.0));
        assert_eq!(agg.last_vaccine_ts, None);
        assert_eq!(agg.last_ambient_ts, Some(expected_short_end_ts));
    }

}
