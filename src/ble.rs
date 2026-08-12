//! Chip-neutral Bluetooth Low Energy GAP configuration contracts.

const LEGACY_ADV_DATA_CAPACITY: usize = 31;

/// Address kind carried by a BLE GAP operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddressType {
    /// Controller-assigned public device address.
    Public,
    /// Static random device address.
    RandomStatic,
    /// Resolvable private address whose identity may be resolved by an IRK.
    ResolvablePrivate,
    /// Non-resolvable private address used only as an ephemeral identity.
    NonResolvablePrivate,
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

    /// Classify and validate a controller-reported random device address.
    ///
    /// The two most-significant bits distinguish non-resolvable private,
    /// resolvable private, and static random addresses. The reserved `10`
    /// pattern is rejected.
    pub const fn random(bytes: [u8; 6]) -> Option<Self> {
        match bytes[5] & 0xc0 {
            0xc0 => Self::random_static(bytes),
            0x40 => {
                let random_part =
                    bytes[3] as u32 | ((bytes[4] as u32) << 8) | (((bytes[5] & 0x3f) as u32) << 16);
                if random_part == 0 || random_part == 0x3f_ffff {
                    None
                } else {
                    Some(Self {
                        bytes,
                        kind: AddressType::ResolvablePrivate,
                    })
                }
            }
            0x00 => {
                if all_equal(bytes, 0) {
                    None
                } else {
                    Some(Self {
                        bytes,
                        kind: AddressType::NonResolvablePrivate,
                    })
                }
            }
            _ => None,
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

/// Whether successful pairing should create a persistent bond.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Bonding {
    /// Encrypt the active link without retaining long-term peer state.
    Disabled,
    /// Retain the peer relationship through an injected bond store.
    Enabled,
}

/// Local input/output capabilities used by Bluetooth pairing association.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IoCapability {
    /// The device can display a value but cannot confirm or enter one.
    DisplayOnly,
    /// The device can display a value and accept a yes/no confirmation.
    DisplayYesNo,
    /// The device can enter a value but cannot display one.
    KeyboardOnly,
    /// The device has no pairing input or output capability.
    NoInputNoOutput,
    /// The device can both display and enter a value.
    KeyboardDisplay,
}

/// Minimum link-security property requested from the BLE host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecurityRequirement {
    /// Unauthenticated pairing followed by link encryption.
    Encrypted,
    /// Authenticated pairing followed by link encryption.
    Authenticated,
    /// Authenticated LE Secure Connections pairing and encryption.
    SecureConnectionsAuthenticated,
}

/// Validated BLE pairing policy.
///
/// The chip adapter maps this semantic policy to its own GAP security values;
/// raw controller encodings and key bytes never cross this boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SecurityConfig {
    bonding: Bonding,
    io_capability: IoCapability,
    requirement: SecurityRequirement,
}

impl SecurityConfig {
    /// Construct an explicit pairing policy.
    pub const fn new(
        bonding: Bonding,
        io_capability: IoCapability,
        requirement: SecurityRequirement,
    ) -> Self {
        Self {
            bonding,
            io_capability,
            requirement,
        }
    }

    /// Whether a successful pairing should be retained.
    pub const fn bonding(self) -> Bonding {
        self.bonding
    }

    /// Local association-model capability.
    pub const fn io_capability(self) -> IoCapability {
        self.io_capability
    }

    /// Minimum security property required for completion.
    pub const fn requirement(self) -> SecurityRequirement {
        self.requirement
    }
}

/// Pairing lifecycle state reported without exposing vendor encodings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingState {
    /// The peer has no active or completed pairing relationship.
    NotPaired,
    /// A pairing operation is in progress.
    Pairing,
    /// Pairing completed successfully for the active peer.
    Paired,
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

/// Bluetooth GATT UUID without controller-specific byte layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GattUuid {
    /// Bluetooth 16-bit UUID.
    Uuid16(u16),
    /// Full 128-bit UUID in network byte order.
    Uuid128([u8; 16]),
}

/// Valid GATT attribute permissions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GattPermissions(u8);

impl GattPermissions {
    /// Attribute can be read by the peer.
    pub const READ: Self = Self(1 << 0);
    /// Attribute can be written by the peer.
    pub const WRITE: Self = Self(1 << 1);

    /// Combine independently named permissions.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Test whether all requested permissions are present.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Return the reviewed portable permission bitmap for a chip adapter.
    #[doc(hidden)]
    pub const fn __bits(self) -> u8 {
        self.0
    }
}

/// Valid GATT characteristic operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GattProperties(u8);

impl GattProperties {
    /// Peer may read the characteristic value.
    pub const READ: Self = Self(1 << 0);
    /// Peer may write and receive a response.
    pub const WRITE: Self = Self(1 << 1);
    /// Peer may write without a response.
    pub const WRITE_WITHOUT_RESPONSE: Self = Self(1 << 2);
    /// Server may send notifications.
    pub const NOTIFY: Self = Self(1 << 3);
    /// Server may send acknowledged indications.
    pub const INDICATE: Self = Self(1 << 4);

    /// Combine independently named properties.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Test whether all requested properties are present.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Return the reviewed portable property bitmap for a chip adapter.
    #[doc(hidden)]
    pub const fn __bits(self) -> u8 {
        self.0
    }
}

/// Static GATT descriptor definition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GattDescriptorDefinition {
    uuid: GattUuid,
    permissions: GattPermissions,
    initial_value: &'static [u8],
    maximum_len: u16,
}

impl GattDescriptorDefinition {
    /// Validate a descriptor's initial and maximum value lengths.
    pub const fn try_new(
        uuid: GattUuid,
        permissions: GattPermissions,
        initial_value: &'static [u8],
        maximum_len: u16,
    ) -> Option<Self> {
        if maximum_len == 0 || initial_value.len() > maximum_len as usize {
            None
        } else {
            Some(Self {
                uuid,
                permissions,
                initial_value,
                maximum_len,
            })
        }
    }

    /// Descriptor UUID.
    pub const fn uuid(self) -> GattUuid {
        self.uuid
    }

    /// Peer permissions.
    pub const fn permissions(self) -> GattPermissions {
        self.permissions
    }

    /// Initial descriptor bytes copied into backend-owned storage.
    pub const fn initial_value(self) -> &'static [u8] {
        self.initial_value
    }

    /// Maximum accepted value length.
    pub const fn maximum_len(self) -> u16 {
        self.maximum_len
    }
}

/// Static GATT characteristic definition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GattCharacteristicDefinition {
    uuid: GattUuid,
    permissions: GattPermissions,
    properties: GattProperties,
    initial_value: &'static [u8],
    maximum_len: u16,
    descriptors: &'static [GattDescriptorDefinition],
}

impl GattCharacteristicDefinition {
    /// Validate value capacity and the notification/indication CCC contract.
    pub const fn try_new(
        uuid: GattUuid,
        permissions: GattPermissions,
        properties: GattProperties,
        initial_value: &'static [u8],
        maximum_len: u16,
        descriptors: &'static [GattDescriptorDefinition],
    ) -> Option<Self> {
        if maximum_len == 0 || initial_value.len() > maximum_len as usize {
            return None;
        }
        let publishes = properties.contains(GattProperties::NOTIFY)
            || properties.contains(GattProperties::INDICATE);
        if publishes && !has_ccc_descriptor(descriptors) {
            return None;
        }
        Some(Self {
            uuid,
            permissions,
            properties,
            initial_value,
            maximum_len,
            descriptors,
        })
    }

    /// Characteristic UUID.
    pub const fn uuid(self) -> GattUuid {
        self.uuid
    }

    /// Peer permissions.
    pub const fn permissions(self) -> GattPermissions {
        self.permissions
    }

    /// Supported GATT operations.
    pub const fn properties(self) -> GattProperties {
        self.properties
    }

    /// Initial value copied into backend-owned storage.
    pub const fn initial_value(self) -> &'static [u8] {
        self.initial_value
    }

    /// Maximum accepted value length.
    pub const fn maximum_len(self) -> u16 {
        self.maximum_len
    }

    /// Static descriptor definitions.
    pub const fn descriptors(self) -> &'static [GattDescriptorDefinition] {
        self.descriptors
    }
}

const fn has_ccc_descriptor(descriptors: &[GattDescriptorDefinition]) -> bool {
    let mut index = 0;
    while index < descriptors.len() {
        if matches!(descriptors[index].uuid, GattUuid::Uuid16(0x2902)) {
            return true;
        }
        index += 1;
    }
    false
}

/// Static GATT service definition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GattServiceDefinition {
    uuid: GattUuid,
    primary: bool,
    characteristics: &'static [GattCharacteristicDefinition],
}

impl GattServiceDefinition {
    /// Require at least one characteristic in a service.
    pub const fn try_new(
        uuid: GattUuid,
        primary: bool,
        characteristics: &'static [GattCharacteristicDefinition],
    ) -> Option<Self> {
        if characteristics.is_empty() {
            None
        } else {
            Some(Self {
                uuid,
                primary,
                characteristics,
            })
        }
    }

    /// Service UUID.
    pub const fn uuid(self) -> GattUuid {
        self.uuid
    }

    /// Whether this is a primary service.
    pub const fn is_primary(self) -> bool {
        self.primary
    }

    /// Static characteristic definitions.
    pub const fn characteristics(self) -> &'static [GattCharacteristicDefinition] {
        self.characteristics
    }
}

/// Complete caller-owned static GATT server database.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GattServerDefinition {
    app_uuid: GattUuid,
    services: &'static [GattServiceDefinition],
}

impl GattServerDefinition {
    /// Require at least one service in the static database.
    pub const fn try_new(
        app_uuid: GattUuid,
        services: &'static [GattServiceDefinition],
    ) -> Option<Self> {
        if services.is_empty() {
            None
        } else {
            Some(Self { app_uuid, services })
        }
    }

    /// Application UUID used to register the server.
    pub const fn app_uuid(self) -> GattUuid {
        self.app_uuid
    }

    /// Static service definitions.
    pub const fn services(self) -> &'static [GattServiceDefinition] {
        self.services
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
        assert!(BluetoothAddress::random([1, 2, 3, 4, 5, 0x80]).is_none());
        assert!(BluetoothAddress::random([0; 6]).is_none());
        assert!(AdvertisingInterval::try_from_units(0x1f).is_none());
        let interval = ScanInterval::try_from_units(0x20).unwrap();
        let window = ScanInterval::try_from_units(0x30).unwrap();
        assert!(ScanTiming::try_new(interval, window).is_none());
    }

    #[test]
    fn classifies_controller_random_addresses() {
        let static_address = BluetoothAddress::random([1, 2, 3, 4, 5, 0xc0]).unwrap();
        assert_eq!(static_address.address_type(), AddressType::RandomStatic);

        let resolvable = BluetoothAddress::random([1, 2, 3, 4, 5, 0x40]).unwrap();
        assert_eq!(resolvable.address_type(), AddressType::ResolvablePrivate);

        let non_resolvable = BluetoothAddress::random([1, 2, 3, 4, 5, 0x00]).unwrap();
        assert_eq!(
            non_resolvable.address_type(),
            AddressType::NonResolvablePrivate
        );
    }

    #[test]
    fn bounds_legacy_advertising_data() {
        assert!(AdvertisingPayload::try_from_slice(&[0; 31]).is_some());
        assert!(AdvertisingPayload::try_from_slice(&[0; 32]).is_none());
        assert!(AdvertisingChannels::try_from_bits(0).is_none());
        assert!(AdvertisingChannels::try_from_bits(0x08).is_none());
    }

    #[test]
    fn security_config_preserves_explicit_policy() {
        const CONFIG: SecurityConfig = SecurityConfig::new(
            Bonding::Enabled,
            IoCapability::DisplayYesNo,
            SecurityRequirement::SecureConnectionsAuthenticated,
        );

        assert_eq!(CONFIG.bonding(), Bonding::Enabled);
        assert_eq!(CONFIG.io_capability(), IoCapability::DisplayYesNo);
        assert_eq!(
            CONFIG.requirement(),
            SecurityRequirement::SecureConnectionsAuthenticated
        );
        assert_eq!(PairingState::NotPaired, PairingState::NotPaired);
    }

    #[test]
    fn validates_static_gatt_database_relations() {
        const CCC: GattDescriptorDefinition = GattDescriptorDefinition::try_new(
            GattUuid::Uuid16(0x2902),
            GattPermissions::READ.union(GattPermissions::WRITE),
            &[0, 0],
            2,
        )
        .unwrap();
        const CHARACTERISTIC: GattCharacteristicDefinition = GattCharacteristicDefinition::try_new(
            GattUuid::Uuid16(0xabcd),
            GattPermissions::READ.union(GattPermissions::WRITE),
            GattProperties::READ
                .union(GattProperties::WRITE)
                .union(GattProperties::NOTIFY),
            b"U3",
            16,
            &[CCC],
        )
        .unwrap();
        const SERVICE: GattServiceDefinition =
            GattServiceDefinition::try_new(GattUuid::Uuid16(0xcdef), true, &[CHARACTERISTIC])
                .unwrap();
        const DATABASE: GattServerDefinition =
            GattServerDefinition::try_new(GattUuid::Uuid16(0xb301), &[SERVICE]).unwrap();

        assert_eq!(DATABASE.services()[0], SERVICE);
        assert!(
            GattCharacteristicDefinition::try_new(
                GattUuid::Uuid16(1),
                GattPermissions::READ,
                GattProperties::NOTIFY,
                &[],
                1,
                &[],
            )
            .is_none()
        );
        assert!(GattServiceDefinition::try_new(GattUuid::Uuid16(1), true, &[]).is_none());
    }
}
