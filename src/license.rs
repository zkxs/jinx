// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

//! logic to validate license activations

use crate::bot::util::SafeDisplay;
use crate::http::jinxxy::LicenseActivation;
use poise::serenity_prelude::UserId;
use regex::RegexSet;
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use std::sync::LazyLock;
use tracing::debug;

// these indices MUST match the array positions in the below RegexSet
const JINXXY_SHORT_KEY_INDEX: usize = 0;
const JINXXY_LONG_KEY_INDEX: usize = 1;
const GUMROAD_KEY_INDEX: usize = 2;
const NUMBER_KEY_INDEX: usize = 3;
const PAYHIP_KEY_INDEX: usize = 4;
const JINXXY_TRANSACTION_ID_INDEX: usize = 5;
static GLOBAL_ANY_LICENSE_REGEX: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        r"(?i)^[A-Z]{4}-[a-f0-9]{12}$", // jinxxy short key `XXXX-cd071c534191`
        r"(?i)^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$", // jinxxy long key `3642d957-c5d8-4d18-a1ae-cd071c534191`. This is a version 4 DCE 1.1, ISO/IEC 11578:1996 UUID.
        r"^[A-F0-9]{8}-[A-F0-9]{8}-[A-F0-9]{8}-[A-F0-9]{8}$", // gumroad key `ABCD1234-1234FEDC-0987A321-A2B3C5D6`
        r"^[0-9]+$",                                          // an integer number `3245554511053325533`
        r"^[A-Z0-9]{5}-[A-Z0-9]{5}-[A-Z0-9]{5}-[A-Z0-9]{5}$", // payhip key `WTKP4-66NL5-HMKQW-GFSCZ`
        r"^pi_[A-Za-z0-9]{24}$",                              // Jinxxy transaction ID `pi_3eAsf8AfuGlZm49dadf3224f`
    ])
    .expect("Failed to compile license heuristic RegexSet")
}); // in case you are wondering the above are not real keys: they're only examples

pub const LOCKING_USER_ID: u64 = 0;

thread_local! {
    // trick to avoid a subtle performance edge case: https://docs.rs/regex/latest/regex/index.html#sharing-a-regex-across-threads-can-result-in-contention
    static ANY_LICENSE_REGEX: RegexSet = GLOBAL_ANY_LICENSE_REGEX.clone();
}

/// All known types of Jinxxy license, as well as some other known types of license users are liable to mistakenly provide.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum LicenseType {
    JinxxyShort,
    JinxxyLong,
    Gumroad,
    Integer,
    Payhip,
    JinxxyTransactionId,
    Unknown,
    /// Not possible under current regex set, but we have the logic for it anyway
    Ambiguous,
}

impl LicenseType {
    /// If the license is either type of Jinnxy key (short or long)
    pub fn is_jinxxy_license(&self) -> bool {
        matches!(self, LicenseType::JinxxyShort | LicenseType::JinxxyLong)
    }

    /// If the license is any known type of license
    pub fn is_license(&self) -> bool {
        !matches!(self, LicenseType::Unknown)
    }

    /// If the license is an integer. This indicates it *may* be a Jinxxy license ID.
    pub fn is_integer(&self) -> bool {
        matches!(self, LicenseType::Integer)
    }

    /// Create the correct type of Jinxxy license for the user-provided value.
    ///
    /// This function only returns Short/Long Jinxxy keys. We intentionally do not create IDs here,
    /// as in the future we may expose IDs in partially untrusted logs, so it'd be bad if IDs could
    /// be used to register a license.
    pub fn create_untrusted_jinxxy_license<'a>(&self, license: &'a str) -> Option<LicenseKey<'a>> {
        match self {
            LicenseType::JinxxyLong => Some(LicenseKey::from_long(license)),
            LicenseType::JinxxyShort => Some(LicenseKey::from_short(license)),
            _ => None,
        }
    }

    /// Create the correct type of Jinxxy license for the user-provided value.
    ///
    /// This function can return any type of Jinxxy key/id.
    pub fn create_trusted_jinxxy_license<'a>(&self, license: &'a str) -> Option<LicenseKey<'a>> {
        match self {
            LicenseType::JinxxyLong => Some(LicenseKey::from_long(license)),
            LicenseType::JinxxyShort => Some(LicenseKey::from_short(license)),
            LicenseType::Integer => Some(LicenseKey::from_id(license)),
            _ => None,
        }
    }

    /// Attempt to figure out what flavor of license we've been provided
    pub fn identify(license: &str) -> Self {
        let matches = ANY_LICENSE_REGEX.with(|regex_set| regex_set.matches(license));
        let mut match_iter = matches.iter();
        // get license type for the first match
        let license_type = match match_iter.next() {
            Some(JINXXY_SHORT_KEY_INDEX) => LicenseType::JinxxyShort,
            Some(JINXXY_LONG_KEY_INDEX) => LicenseType::JinxxyLong,
            Some(GUMROAD_KEY_INDEX) => LicenseType::Gumroad,
            Some(NUMBER_KEY_INDEX) => LicenseType::Integer,
            Some(PAYHIP_KEY_INDEX) => LicenseType::Payhip,
            Some(JINXXY_TRANSACTION_ID_INDEX) => LicenseType::JinxxyTransactionId,
            _ => LicenseType::Unknown,
        };

        if match_iter.next().is_some() {
            debug!("{} ambiguous matches for \"{}\": {:?}", matches.len(), license, matches);
            LicenseType::Ambiguous
        } else {
            license_type
        }
    }
}

// display impl for my logs
impl Display for LicenseType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            LicenseType::JinxxyShort => write!(f, "a Jinxxy short key"),
            LicenseType::JinxxyLong => write!(f, "a Jinxxy long key"),
            LicenseType::Gumroad => write!(f, "a Gumroad key"),
            LicenseType::Integer => write!(f, "a number"),
            LicenseType::Payhip => write!(f, "a Payhip key"),
            LicenseType::JinxxyTransactionId => write!(f, "a Jinxxy transaction ID"),
            LicenseType::Unknown => write!(f, "an unknown value"),
            LicenseType::Ambiguous => write!(f, "an ambiguous value"),
        }
    }
}

impl SafeDisplay<'_, DisplayLicenseType> for LicenseType {
    fn safe_display(&'_ self) -> DisplayLicenseType {
        DisplayLicenseType(*self)
    }
}

pub struct DisplayLicenseType(LicenseType);

// display impl shown to users
impl Display for DisplayLicenseType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            LicenseType::JinxxyShort => write!(f, "a **Jinxxy short key**"),
            LicenseType::JinxxyLong => write!(f, "a **Jinxxy long key**"),
            LicenseType::Gumroad => write!(f, "a **Gumroad key**"),
            LicenseType::Integer => write!(f, "an **unknown value**"), // the word "integer" is scary
            LicenseType::Payhip => write!(f, "a **Payhip key**"),
            LicenseType::JinxxyTransactionId => write!(f, "a **transaction ID**"), // don't admit this is a Jinxxy thing
            LicenseType::Unknown => write!(f, "an **unknown value**"),
            LicenseType::Ambiguous => write!(f, "an **ambiguous value**"),
        }
    }
}

/// Represents all allowed license formats usable by the Jinxxy API
#[derive(Clone)]
pub enum LicenseKey<'a> {
    Id(LicenseKeyInner<'a>),
    Short(LicenseKeyInner<'a>),
    Long(LicenseKeyInner<'a>),
}

/// stupid hack to prevent `pub enum LicenseKey` users from constructing it with invalid variants
#[derive(Clone)]
pub struct LicenseKeyInner<'a>(Cow<'a, str>);

impl<'a> LicenseKeyInner<'a> {
    pub fn get(self) -> Cow<'a, str> {
        self.0
    }

    pub fn into_string(self) -> String {
        self.0.into_owned()
    }
}

impl<'a> LicenseKey<'a> {
    /// this is a no-op wrapper, as ids are fully numeric and therefore do not have case
    pub fn from_id(id: &'a str) -> Self {
        LicenseKey::Id(LicenseKeyInner(Cow::Borrowed(id)))
    }

    /// ensures all short keys get case like `XXXX-cd071c534191`
    pub fn from_short(license: &'a str) -> Self {
        let bytes = license.as_bytes();
        if bytes[0..4].iter().any(|b| *b != b'X') || bytes[5..].iter().any(|b| b.is_ascii_uppercase()) {
            // either the first 4 characters contained a non-'X' or the remaining characters contained an uppercase
            let mut owned = bytes.to_vec();
            owned[0..4].fill(b'X');
            owned[5..].make_ascii_lowercase();
            LicenseKey::Short(LicenseKeyInner(Cow::Owned(
                String::from_utf8(owned).expect("short license should always be valid UTF-8"),
            )))
        } else {
            // there were no mis-cased characters, so we just proxy the input back out
            LicenseKey::Short(LicenseKeyInner(Cow::Borrowed(license)))
        }
    }

    /// ensures all long keys are fully lowercase
    pub fn from_long(license: &'a str) -> Self {
        // fine to treat this as bytes, as the long regex only allows ASCII characters
        if license.as_bytes().iter().any(|c| c.is_ascii_uppercase()) {
            // there was an uppercase character, so we copy the string and lowercase it
            LicenseKey::Long(LicenseKeyInner(Cow::Owned(license.to_ascii_lowercase())))
        } else {
            // there were no uppercase characters, so we just proxy the input back out
            LicenseKey::Long(LicenseKeyInner(Cow::Borrowed(license)))
        }
    }

    /// Helper function for our unit tests
    #[cfg(test)]
    fn get(self) -> Cow<'a, str> {
        match self {
            LicenseKey::Id(inner) | LicenseKey::Short(inner) | LicenseKey::Long(inner) => inner.get(),
        }
    }
}

/// Results of an activation validation check
#[derive(Default)] // by default nothing gets set
pub struct ActivationValidation {
    /// If the expected user has activated the license
    pub own_user: bool,
    /// If the expected user has activated the license more than once (this shouldn't be possible)
    pub multiple: bool,
    /// If an unexpected user has activated the license
    pub other_user: bool,
    /// If the license is locked (otherwise valid, but forbidden from being used to grant roles)
    pub locked: bool,
}

impl ActivationValidation {
    /// Check if the license has multiple conflicting activations (this shouldn't be possible)
    pub fn deadlocked(&self) -> bool {
        self.own_user && self.other_user
    }

    /// Run validation checks on Jinxxy license activations
    /// - `expected_user_id` - user we expect to have activated
    /// - `activations` - all known activations
    pub fn new(expected_user_id: UserId, activations: &[LicenseActivation]) -> Self {
        Self::validate_activated_users(
            expected_user_id,
            activations
                .iter()
                .filter_map(|activation| activation.try_into_user_id()),
        )
    }

    /// Run validation checks on license activations
    /// - `expected_user_id` - user we expect to have activated
    /// - `user_ids` - user ids from all known activations
    fn validate_activated_users(expected_user_id: UserId, user_ids: impl Iterator<Item = u64>) -> Self {
        let mut own_user = false;
        let mut multiple = false;
        let mut other_user = false;
        let mut locked = false;

        for user_id in user_ids {
            if user_id == expected_user_id.get() {
                // check if this is NOT the first activation
                if own_user {
                    multiple = true;
                }
                // expected user activated
                own_user = true;
            } else if user_id == LOCKING_USER_ID {
                locked = true;
            } else {
                // unexpected user activated
                other_user = true;
            }
        }

        ActivationValidation {
            own_user,
            multiple,
            other_user,
            locked,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use tracing_test::traced_test;

    #[test]
    #[traced_test]
    fn test_jinxxy_short_license() {
        let input = "XXXX-cd071c534191";
        let expected = input;
        let license_type = LicenseType::identify(input);
        assert_eq!(license_type, LicenseType::JinxxyShort);
        assert_eq!(
            license_type.create_untrusted_jinxxy_license(input).map(|s| s.get()),
            Some(Cow::Borrowed(expected))
        );
        assert_eq!(
            license_type.create_trusted_jinxxy_license(input).map(|s| s.get()),
            Some(Cow::Borrowed(expected))
        );
    }

    #[test]
    #[traced_test]
    fn test_jinxxy_short_license_fucked_case_reverse() {
        let input = "xxxx-CD071C534191";
        let expected = "XXXX-cd071c534191";
        let license_type = LicenseType::identify(input);
        assert_eq!(license_type, LicenseType::JinxxyShort);
        assert_eq!(
            license_type.create_untrusted_jinxxy_license(input).map(|s| s.get()),
            Some(Cow::Borrowed(expected))
        );
        assert_eq!(
            license_type.create_trusted_jinxxy_license(input).map(|s| s.get()),
            Some(Cow::Borrowed(expected))
        );
    }

    #[test]
    #[traced_test]
    fn test_jinxxy_short_license_fucked_case_lower() {
        let input = "xxxx-cd071c534191";
        let expected = "XXXX-cd071c534191";
        let license_type = LicenseType::identify(input);
        assert_eq!(license_type, LicenseType::JinxxyShort);
        assert_eq!(
            license_type.create_untrusted_jinxxy_license(input).map(|s| s.get()),
            Some(Cow::Borrowed(expected))
        );
        assert_eq!(
            license_type.create_trusted_jinxxy_license(input).map(|s| s.get()),
            Some(Cow::Borrowed(expected))
        );
    }

    #[test]
    #[traced_test]
    fn test_jinxxy_short_license_fucked_case_upper() {
        let input = "XXXX-CD071C534191";
        let expected = "XXXX-cd071c534191";
        let license_type = LicenseType::identify(input);
        assert_eq!(license_type, LicenseType::JinxxyShort);
        assert_eq!(
            license_type.create_untrusted_jinxxy_license(input).map(|s| s.get()),
            Some(Cow::Borrowed(expected))
        );
        assert_eq!(
            license_type.create_trusted_jinxxy_license(input).map(|s| s.get()),
            Some(Cow::Borrowed(expected))
        );
    }

    #[test]
    #[traced_test]
    fn test_jinxxy_long_license() {
        let input = "3642d957-c5d8-4d18-a1ae-cd071c534191";
        let expected = input;
        let license_type = LicenseType::identify(input);
        assert_eq!(license_type, LicenseType::JinxxyLong);
        assert_eq!(
            license_type.create_untrusted_jinxxy_license(input).map(|s| s.get()),
            Some(Cow::Borrowed(expected))
        );
        assert_eq!(
            license_type.create_trusted_jinxxy_license(input).map(|s| s.get()),
            Some(Cow::Borrowed(expected))
        );
    }

    #[test]
    #[traced_test]
    fn test_jinxxy_long_fucked_case() {
        let input = "3642D957-C5D8-4D18-A1AE-CD071C534191";
        let expected = "3642d957-c5d8-4d18-a1ae-cd071c534191";
        let license_type = LicenseType::identify(input);
        assert_eq!(license_type, LicenseType::JinxxyLong);
        assert_eq!(
            license_type.create_untrusted_jinxxy_license(input).map(|s| s.get()),
            Some(Cow::Borrowed(expected))
        );
        assert_eq!(
            license_type.create_trusted_jinxxy_license(input).map(|s| s.get()),
            Some(Cow::Borrowed(expected))
        );
    }

    #[test]
    #[traced_test]
    fn test_integer_license() {
        let input = "3245554511053325533";
        let expected = input;
        let license_type = LicenseType::identify(input);
        assert_eq!(license_type, LicenseType::Integer);
        assert_eq!(
            license_type.create_untrusted_jinxxy_license(input).map(|s| s.get()),
            None
        );
        assert_eq!(
            license_type.create_trusted_jinxxy_license(input).map(|s| s.get()),
            Some(Cow::Borrowed(expected))
        );
    }

    #[test]
    #[traced_test]
    fn test_gumroad_license() {
        assert_eq!(
            LicenseType::identify("ABCD1234-1234FEDC-0987A321-A2B3C5D6"),
            LicenseType::Gumroad
        );
    }

    #[test]
    #[traced_test]
    fn test_payhip_license() {
        assert_eq!(LicenseType::identify("WTKP4-66NL5-HMKQW-GFSCZ"), LicenseType::Payhip);
    }

    #[test]
    #[traced_test]
    fn test_unknown_license() {
        assert_eq!(LicenseType::identify("foo"), LicenseType::Unknown);
    }

    #[test]
    #[traced_test]
    fn test_not_a_license() {
        assert_eq!(LicenseType::identify("bing bong"), LicenseType::Unknown);
    }
}
