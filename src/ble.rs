//! Chip-neutral Bluetooth Low Energy GAP configuration contracts.

const LEGACY_ADV_DATA_CAPACITY: usize = 31;

/// Address kind carried by a BLE GAP operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddressType {
    /// Controller-assigned public device address.
    Public,
    /// Static random device address.
    RandomStatic,
}

/// Validated BLE device address.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BluetoothAddress {
    bytes: [u8; 6],
    kind: AddressType,
}

impl BluetoothAddress {
    /// Validate a non-zero public device address.
    pub const fn public(bytes: [u8; 6]) -> Option<Self> {
        if all_equal(bytes, 0) {
            None
        } else {
            Some(Self {
                bytes,
                kind: AddressType::Public,
            })
        }
    }

    /// Validate a static random address according to the two most-significant bits.
    pub const fn random_static(bytes: [u8; 6]) -> Option<Self> {
        let random_part_is_zero = bytes[0] == 0
            && bytes[1] == 0
            && bytes[2] == 0
            && bytes[3] == 0
            && bytes[4] == 0
            && bytes[5] & 0x3f == 0;
        let random_part_is_one = bytes[0] == 0xff
            && bytes[1] == 0xff
            && bytes[2] == 0xff
            && bytes[3] == 0xff
            && bytes[4] == 0xff
            && bytes[5] & 0x3f == 0x3f;
        if bytes[5] & 0xc0 != 0xc0 || random_part_is_zero || random_part_is_one {
            None
        } else {
            Some(Self {
                bytes,
                kind: AddressType::RandomStatic,
            })
        }
    }

    /// Return the address bytes in controller byte order.
    pub const fn bytes(self) -> [u8; 6] {
        self.bytes
    }

    /// Return the validated address kind.
    pub const fn address_type(self) -> AddressType {
        self.kind
    }
}

const fn all_equal(bytes: [u8; 6], value: u8) -> bool {
    bytes[0] == value
        && bytes[1] == value
        && bytes[2] == value
        && bytes[3] == value
        && bytes[4] == value
        && bytes[5] == value
}

/// Legacy advertising interval in 0.625 ms controller units.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AdvertisingInterval(u16);

impl AdvertisingInterval {
    /// Validate the Bluetooth legacy-advertising interval range.
    pub const fn try_from_units(units: u16) -> Option<Self> {
        if units < 0x20 || units > 0x4000 {
            None
        } else {
            Some(Self(units))
        }
    }

    /// Return the 0.625 ms controller units.
    pub const fn as_units(self) -> u16 {
        self.0
    }
}

/// Ordered advertising interval range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdvertisingTiming {
    minimum: AdvertisingInterval,
    maximum: AdvertisingInterval,
}

impl AdvertisingTiming {
    /// Validate that the minimum does not exceed the maximum.
    pub const fn try_new(
        minimum: AdvertisingInterval,
        maximum: AdvertisingInterval,
    ) -> Option<Self> {
        if minimum.0 > maximum.0 {
            None
        } else {
            Some(Self { minimum, maximum })
        }
    }

    /// Smallest selected interval.
    pub const fn minimum(self) -> AdvertisingInterval {
        self.minimum
    }

    /// Largest selected interval.
    pub const fn maximum(self) -> AdvertisingInterval {
        self.maximum
    }
}

/// Non-empty subset of the three primary advertising channels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdvertisingChannels(u8);

impl AdvertisingChannels {
    /// Channels 37, 38, and 39.
    pub const ALL: Self = Self(0x07);

    /// Validate a controller channel bitmap.
    pub const fn try_from_bits(bits: u8) -> Option<Self> {
        if bits == 0 || bits & !0x07 != 0 {
            None
        } else {
            Some(Self(bits))
        }
    }

    /// Return the three-bit controller bitmap.
    pub const fn bits(self) -> u8 {
        self.0
    }
}

/// Owned, bounded legacy advertising payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdvertisingPayload {
    bytes: [u8; LEGACY_ADV_DATA_CAPACITY],
    len: u8,
}

impl AdvertisingPayload {
    /// Copy at most 31 bytes into fixed-capacity storage.
    pub fn try_from_slice(value: &[u8]) -> Option<Self> {
        if value.len() > LEGACY_ADV_DATA_CAPACITY {
            return None;
        }
        let mut bytes = [0; LEGACY_ADV_DATA_CAPACITY];
        bytes[..value.len()].copy_from_slice(value);
        Some(Self {
            bytes,
            len: value.len() as u8,
        })
    }

    /// Exact payload bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

/// Typed legacy advertising request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdvertisingConfig {
    timing: AdvertisingTiming,
    channels: AdvertisingChannels,
    payload: AdvertisingPayload,
}

impl AdvertisingConfig {
    /// Construct a request from independently validated fields.
    pub const fn new(
        timing: AdvertisingTiming,
        channels: AdvertisingChannels,
        payload: AdvertisingPayload,
    ) -> Self {
        Self {
            timing,
            channels,
            payload,
        }
    }

    /// Selected timing range.
    pub const fn timing(self) -> AdvertisingTiming {
        self.timing
    }

    /// Selected primary channels.
    pub const fn channels(self) -> AdvertisingChannels {
        self.channels
    }

    /// Borrow the bounded payload.
    pub const fn payload(&self) -> &AdvertisingPayload {
        &self.payload
    }
}

/// Scan interval or window in 0.625 ms controller units.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ScanInterval(u16);

impl ScanInterval {
    /// Validate the controller scan timing range.
    pub const fn try_from_units(units: u16) -> Option<Self> {
        if units < 0x0004 || units > 0x4000 {
            None
        } else {
            Some(Self(units))
        }
    }

    /// Return the 0.625 ms controller units.
    pub const fn as_units(self) -> u16 {
        self.0
    }
}

/// Scan interval and window with `window <= interval` guaranteed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScanTiming {
    interval: ScanInterval,
    window: ScanInterval,
}

impl ScanTiming {
    /// Validate one scan timing pair.
    pub const fn try_new(interval: ScanInterval, window: ScanInterval) -> Option<Self> {
        if window.0 > interval.0 {
            None
        } else {
            Some(Self { interval, window })
        }
    }

    /// Repetition interval.
    pub const fn interval(self) -> ScanInterval {
        self.interval
    }

    /// Active scan window.
    pub const fn window(self) -> ScanInterval {
        self.window
    }
}

/// Host scan behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanMode {
    /// Listen without sending scan requests.
    Passive,
    /// Send scan requests when the peer supports them.
    Active,
}

/// Typed BLE scan request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScanConfig {
    timing: ScanTiming,
    mode: ScanMode,
    filter_duplicates: bool,
}

impl ScanConfig {
    /// Construct a request from validated timing and explicit behavior.
    pub const fn new(timing: ScanTiming, mode: ScanMode, filter_duplicates: bool) -> Self {
        Self {
            timing,
            mode,
            filter_duplicates,
        }
    }

    /// Selected scan timing.
    pub const fn timing(self) -> ScanTiming {
        self.timing
    }

    /// Active or passive behavior.
    pub const fn mode(self) -> ScanMode {
        self.mode
    }

    /// Whether duplicate reports should be filtered.
    pub const fn filter_duplicates(self) -> bool {
        self.filter_duplicates
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_addresses_and_timing() {
        assert!(BluetoothAddress::public([0; 6]).is_none());
        assert!(BluetoothAddress::random_static([1, 2, 3, 4, 5, 0x80]).is_none());
        assert!(BluetoothAddress::random_static([0, 0, 0, 0, 0, 0xc0]).is_none());
        assert!(BluetoothAddress::random_static([0xff; 6]).is_none());
        assert!(AdvertisingInterval::try_from_units(0x1f).is_none());
        let interval = ScanInterval::try_from_units(0x20).unwrap();
        let window = ScanInterval::try_from_units(0x30).unwrap();
        assert!(ScanTiming::try_new(interval, window).is_none());
    }

    #[test]
    fn bounds_legacy_advertising_data() {
        assert!(AdvertisingPayload::try_from_slice(&[0; 31]).is_some());
        assert!(AdvertisingPayload::try_from_slice(&[0; 32]).is_none());
        assert!(AdvertisingChannels::try_from_bits(0).is_none());
        assert!(AdvertisingChannels::try_from_bits(0x08).is_none());
    }
}
