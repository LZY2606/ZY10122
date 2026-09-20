//! Explicit units shared by the whole tool.
//!
//! Every persisted distance is metres, every time is seconds, every delay is
//! seconds and the sound speed is metres per second. The API layer repeats the
//! unit in its JSON (`"coordinate_unit": "m"`, ...) so that a consumer can
//! never mistake millisecond fields for microseconds.

pub const COORDINATE_UNIT: &str = "m";
pub const TIME_UNIT: &str = "s";
pub const DELAY_UNIT: &str = "s";
pub const SPEED_UNIT: &str = "m/s";

/// Event-wide physical constants. Defaults are dry air near 20 deg C.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhysicalModel {
    pub sound_speed: f64,
    /// Radius around the origin outside of which a numerical estimate is
    /// rejected instead of being pulled back on to the boundary.
    pub observable_radius: f64,
}

impl Default for PhysicalModel {
    fn default() -> Self {
        Self {
            sound_speed: 343.0,
            observable_radius: 2000.0,
        }
    }
}

impl PhysicalModel {
    pub fn validate(&self) -> Result<(), String> {
        if !(self.sound_speed.is_finite() && self.sound_speed > 50.0 && self.sound_speed < 2000.0) {
            return Err("sound_speed must be finite and within 50..2000 m/s".into());
        }
        if !(self.observable_radius.is_finite() && self.observable_radius > 0.0) {
            return Err("observable_radius must be a positive finite number of metres".into());
        }
        Ok(())
    }
}
