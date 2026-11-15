//! The line settings a serial session is opened with (R21).
//!
//! R21 names the five settings a user configures - baud rate, data bits, parity,
//! stop bits and flow control - and asks that an impossible combination be refused
//! before anything is opened. That is what [`SerialConfig::validate`] does: it refuses
//! what no driver could accept (a zero baud rate, five data bits with two stop bits),
//! so the failure is reported as a wrong setting instead of as a mysterious driver
//! error. Whether a device accepts a rate or a flow control the platform knows about
//! is still decided by the driver, and its refusal is reported with its own code.

use crate::error::{SerialCause, SerialError};

/// How many bits one character carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DataBits {
    /// Five bits per character.
    Five,
    /// Six bits per character.
    Six,
    /// Seven bits per character.
    Seven,
    /// Eight bits per character.
    Eight,
}

impl DataBits {
    /// The number of bits, as the driver wants it.
    #[must_use]
    pub fn bits(self) -> u8 {
        match self {
            Self::Five => 5,
            Self::Six => 6,
            Self::Seven => 7,
            Self::Eight => 8,
        }
    }

    /// The digit shown in a "8N1" style summary.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Five => "5",
            Self::Six => "6",
            Self::Seven => "7",
            Self::Eight => "8",
        }
    }

    /// Every setting, in the order a form should offer them.
    #[must_use]
    pub fn all() -> &'static [Self] {
        &[Self::Five, Self::Six, Self::Seven, Self::Eight]
    }
}

/// How the parity bit is formed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Parity {
    /// No parity bit.
    None,
    /// The parity bit makes the number of set bits odd.
    Odd,
    /// The parity bit makes the number of set bits even.
    Even,
    /// The parity bit is always one.
    Mark,
    /// The parity bit is always zero.
    Space,
}

impl Parity {
    /// The letter shown in a "8N1" style summary.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "N",
            Self::Odd => "O",
            Self::Even => "E",
            Self::Mark => "M",
            Self::Space => "S",
        }
    }

    /// Every setting, in the order a form should offer them.
    #[must_use]
    pub fn all() -> &'static [Self] {
        &[Self::None, Self::Odd, Self::Even, Self::Mark, Self::Space]
    }
}

/// How many stop bits end a character.
///
/// The one-and-a-half variant only exists for five data bits and no current driver
/// accepts it, so it is not offered; five data bits with two stop bits is refused by
/// [`SerialConfig::validate`] and by the driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StopBits {
    /// One stop bit.
    One,
    /// Two stop bits.
    Two,
}

impl StopBits {
    /// The digit shown in a "8N1" style summary.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::One => "1",
            Self::Two => "2",
        }
    }

    /// Every setting, in the order a form should offer them.
    #[must_use]
    pub fn all() -> &'static [Self] {
        &[Self::One, Self::Two]
    }
}

/// Which flow control keeps the two ends from overrunning each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FlowControl {
    /// No flow control: bytes go out as fast as the port accepts them.
    None,
    /// XON/XOFF characters in the data stream.
    Software,
    /// The RTS and CTS lines.
    Hardware,
}

impl FlowControl {
    /// A label for a summary or a form.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Software => "XON/XOFF",
            Self::Hardware => "RTS/CTS",
        }
    }

    /// Every setting, in the order a form should offer them.
    #[must_use]
    pub fn all() -> &'static [Self] {
        &[Self::None, Self::Software, Self::Hardware]
    }
}

/// The rates a device is most likely to accept, for a form's list.
///
/// The list is a convenience for the interface, not a limit: [`SerialConfig`] keeps
/// whatever rate the user typed, and the driver decides whether it can produce it.
pub const COMMON_BAUD_RATES: &[u32] = &[
    300, 600, 1200, 2400, 4800, 9600, 14400, 19200, 38400, 57_600, 115_200, 230_400, 460_800,
    921_600,
];

/// The fastest rate this crate will ask a driver for.
///
/// Above this a serial line is no longer a serial line; refusing the value here keeps
/// a typo from becoming an unsupported-baud error from the driver.
pub const MAX_BAUD_RATE: u32 = 4_000_000;

/// One complete set of line settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SerialConfig {
    baud: u32,
    data_bits: DataBits,
    parity: Parity,
    stop_bits: StopBits,
    flow: FlowControl,
}

impl SerialConfig {
    /// Builds a set of settings. Use [`SerialConfig::validate`] before opening.
    #[must_use]
    pub const fn new(
        baud: u32,
        data_bits: DataBits,
        parity: Parity,
        stop_bits: StopBits,
        flow: FlowControl,
    ) -> Self {
        Self {
            baud,
            data_bits,
            parity,
            stop_bits,
            flow,
        }
    }

    /// The baud rate.
    #[must_use]
    pub const fn baud(&self) -> u32 {
        self.baud
    }

    /// The data bits per character.
    #[must_use]
    pub const fn data_bits(&self) -> DataBits {
        self.data_bits
    }

    /// The parity bit.
    #[must_use]
    pub const fn parity(&self) -> Parity {
        self.parity
    }

    /// The stop bits per character.
    #[must_use]
    pub const fn stop_bits(&self) -> StopBits {
        self.stop_bits
    }

    /// The flow control.
    #[must_use]
    pub const fn flow(&self) -> FlowControl {
        self.flow
    }

    /// Returns the same settings with another baud rate.
    #[must_use]
    pub const fn with_baud(mut self, baud: u32) -> Self {
        self.baud = baud;
        self
    }

    /// Refuses a combination no driver could accept.
    ///
    /// # Errors
    ///
    /// [`SerialCause::UnsupportedConfig`], with the field that is wrong in the detail,
    /// so the failure can be shown before the port is touched.
    pub fn validate(&self) -> Result<(), SerialError> {
        if self.baud == 0 || self.baud > MAX_BAUD_RATE {
            return Err(
                SerialError::new(SerialCause::UnsupportedConfig).with_detail(format!(
                    "a baud rate of {} is outside 1..={MAX_BAUD_RATE}",
                    self.baud
                )),
            );
        }
        if self.data_bits == DataBits::Five && self.stop_bits == StopBits::Two {
            return Err(SerialError::new(SerialCause::UnsupportedConfig)
                .with_detail("five data bits cannot be paired with two stop bits"));
        }
        Ok(())
    }

    /// The settings as a "115200 8N1, none flow control" summary.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{} {}{}{}, {} flow control",
            self.baud,
            self.data_bits.label(),
            self.parity.label(),
            self.stop_bits.label(),
            self.flow.label()
        )
    }
}

impl Default for SerialConfig {
    /// 115200 8N1 with no flow control: the setting a current USB adapter expects.
    fn default() -> Self {
        Self::new(
            115_200,
            DataBits::Eight,
            Parity::None,
            StopBits::One,
            FlowControl::None,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_115200_8n1() {
        let config = SerialConfig::default();
        assert_eq!(config.baud(), 115_200);
        assert_eq!(config.summary(), "115200 8N1, none flow control");
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn a_zero_or_absurd_baud_rate_is_refused_before_the_port_is_opened() {
        for baud in [0, MAX_BAUD_RATE + 1] {
            let error = SerialConfig::default().with_baud(baud).validate();
            let error = error.expect_err("an impossible rate must be refused");
            assert_eq!(error.cause(), SerialCause::UnsupportedConfig);
            assert!(
                error.detail().unwrap_or_default().contains("baud rate"),
                "{error}"
            );
        }
    }

    #[test]
    fn five_data_bits_with_two_stop_bits_is_refused() {
        let config = SerialConfig::new(
            9600,
            DataBits::Five,
            Parity::None,
            StopBits::Two,
            FlowControl::None,
        );
        let error = config.validate().expect_err("5 data bits + 2 stop bits");
        assert_eq!(error.cause(), SerialCause::UnsupportedConfig);
    }

    #[test]
    fn a_non_default_combination_validates_and_summarises() {
        let config = SerialConfig::new(
            57_600,
            DataBits::Seven,
            Parity::Even,
            StopBits::Two,
            FlowControl::Hardware,
        );
        assert_eq!(config.validate(), Ok(()));
        assert_eq!(config.summary(), "57600 7E2, RTS/CTS flow control");
    }

    #[test]
    fn every_setting_is_offered_with_a_label() {
        for bits in DataBits::all() {
            assert!(!bits.label().is_empty());
            assert!((5..=8).contains(&bits.bits()));
        }
        for parity in Parity::all() {
            assert!(!parity.label().is_empty());
        }
        for stop in StopBits::all() {
            assert!(!stop.label().is_empty());
        }
        for flow in FlowControl::all() {
            assert!(!flow.label().is_empty());
        }
        assert!(COMMON_BAUD_RATES.contains(&115_200));
    }
}
