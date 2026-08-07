//! Chip-neutral SLE announce and seek configuration contracts.

const ANNOUNCE_DATA_CAPACITY: usize = 64;

/// Address kind carried by one SLE operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddressType {
    /// Public device address.
    Public,
    /// Random device address.
    Random,
}

/// Validated non-zero SLE device address.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SleAddress {
    bytes: [u8; 6],
    kind: AddressType,
}

impl SleAddress {
    /// Validate and copy a device address.
    pub const fn try_new(bytes: [u8; 6], kind: AddressType) -> Option<Self> {
        if bytes[0] == 0
            && bytes[1] == 0
            && bytes[2] == 0
            && bytes[3] == 0
            && bytes[4] == 0
            && bytes[5] == 0
        {
            None
        } else {
            Some(Self { bytes, kind })
        }
    }

    /// Return the controller byte order.
    pub const fn bytes(self) -> [u8; 6] {
        self.bytes
    }

    /// Return the validated address kind.
    pub const fn address_type(self) -> AddressType {
        self.kind
    }
}

/// SLE announce interval in 125 us controller units.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AnnounceInterval(u32);

impl AnnounceInterval {
    /// Validate the WS63 public SLE API range without truncating its 24-bit value.
    pub const fn try_from_units(units: u32) -> Option<Self> {
        if units < 0x20 || units > 0x00ff_ffff {
            None
        } else {
            Some(Self(units))
        }
    }

    /// Return controller units.
    pub const fn as_units(self) -> u32 {
        self.0
    }
}

/// Ordered announce timing range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnnounceTiming {
    minimum: AnnounceInterval,
    maximum: AnnounceInterval,
}

impl AnnounceTiming {
    /// Validate that the minimum does not exceed the maximum.
    pub const fn try_new(minimum: AnnounceInterval, maximum: AnnounceInterval) -> Option<Self> {
        if minimum.0 > maximum.0 {
            None
        } else {
            Some(Self { minimum, maximum })
        }
    }

    /// Smallest interval.
    pub const fn minimum(self) -> AnnounceInterval {
        self.minimum
    }

    /// Largest interval.
    pub const fn maximum(self) -> AnnounceInterval {
        self.maximum
    }
}

/// Non-empty subset of the three SLE announce channels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnnounceChannels(u8);

impl AnnounceChannels {
    /// All primary announce channels.
    pub const ALL: Self = Self(0x07);

    /// Validate a three-bit channel map.
    pub const fn try_from_bits(bits: u8) -> Option<Self> {
        if bits == 0 || bits & !0x07 != 0 {
            None
        } else {
            Some(Self(bits))
        }
    }

    /// Return the controller bitmap.
    pub const fn bits(self) -> u8 {
        self.0
    }
}

/// Owned, bounded announce or seek-response payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnnouncePayload {
    bytes: [u8; ANNOUNCE_DATA_CAPACITY],
    len: u8,
}

impl AnnouncePayload {
    /// Copy at most 64 bytes into fixed-capacity storage.
    pub fn try_from_slice(value: &[u8]) -> Option<Self> {
        if value.len() > ANNOUNCE_DATA_CAPACITY {
            return None;
        }
        let mut bytes = [0; ANNOUNCE_DATA_CAPACITY];
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

/// Typed SLE announce request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnnounceConfig {
    timing: AnnounceTiming,
    channels: AnnounceChannels,
    data: AnnouncePayload,
    seek_response: AnnouncePayload,
}

impl AnnounceConfig {
    /// Construct a request from independently validated fields.
    pub const fn new(
        timing: AnnounceTiming,
        channels: AnnounceChannels,
        data: AnnouncePayload,
        seek_response: AnnouncePayload,
    ) -> Self {
        Self {
            timing,
            channels,
            data,
            seek_response,
        }
    }

    /// Selected timing range.
    pub const fn timing(self) -> AnnounceTiming {
        self.timing
    }

    /// Selected channel map.
    pub const fn channels(self) -> AnnounceChannels {
        self.channels
    }

    /// Borrow announce data.
    pub const fn data(&self) -> &AnnouncePayload {
        &self.data
    }

    /// Borrow seek-response data.
    pub const fn seek_response(&self) -> &AnnouncePayload {
        &self.seek_response
    }
}

/// SLE seek interval or window in 125 us controller units.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SeekInterval(u16);

impl SeekInterval {
    /// Validate the public SLE seek timing range.
    pub const fn try_from_units(units: u16) -> Option<Self> {
        if units < 0x14 {
            None
        } else {
            Some(Self(units))
        }
    }

    /// Return controller units.
    pub const fn as_units(self) -> u16 {
        self.0
    }
}

/// Seek interval and window with `window <= interval` guaranteed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SeekTiming {
    interval: SeekInterval,
    window: SeekInterval,
}

impl SeekTiming {
    /// Validate one seek timing pair.
    pub const fn try_new(interval: SeekInterval, window: SeekInterval) -> Option<Self> {
        if window.0 > interval.0 {
            None
        } else {
            Some(Self { interval, window })
        }
    }

    /// Repetition interval.
    pub const fn interval(self) -> SeekInterval {
        self.interval
    }

    /// Active seek window.
    pub const fn window(self) -> SeekInterval {
        self.window
    }
}

/// Typed SLE seek request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SeekConfig {
    timing: SeekTiming,
    filter_duplicates: bool,
}

impl SeekConfig {
    /// Construct a seek request.
    pub const fn new(timing: SeekTiming, filter_duplicates: bool) -> Self {
        Self {
            timing,
            filter_duplicates,
        }
    }

    /// Selected seek timing.
    pub const fn timing(self) -> SeekTiming {
        self.timing
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
    fn validates_addresses_timing_and_channels() {
        assert!(SleAddress::try_new([0; 6], AddressType::Public).is_none());
        assert!(AnnounceInterval::try_from_units(0x1f).is_none());
        assert!(AnnounceInterval::try_from_units(0x0100_0000).is_none());
        let short = AnnounceInterval::try_from_units(0x20).unwrap();
        let long = AnnounceInterval::try_from_units(0x40).unwrap();
        assert!(AnnounceTiming::try_new(long, short).is_none());
        assert!(AnnounceChannels::try_from_bits(0).is_none());
        assert!(AnnounceChannels::try_from_bits(0x08).is_none());
    }

    #[test]
    fn bounds_announce_payloads() {
        assert!(AnnouncePayload::try_from_slice(&[0; 64]).is_some());
        assert!(AnnouncePayload::try_from_slice(&[0; 65]).is_none());
        assert!(SeekInterval::try_from_units(0x13).is_none());
        let interval = SeekInterval::try_from_units(100).unwrap();
        let window = SeekInterval::try_from_units(101).unwrap();
        assert!(SeekTiming::try_new(interval, window).is_none());
    }
}
